//! Browser-facing observer constructors.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static RESIZE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static RESIZE_ENTRY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static RESIZE_SIZE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DOM_RESIZE_OBSERVERS: RefCell<Vec<Rc<RefCell<DomResizeObserverState>>>> =
        const { RefCell::new(Vec::new()) };
    static RESIZE_DEVICE_PIXEL_HOST_WARNING: RefCell<bool> = const { RefCell::new(false) };
    static MUTATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MUTATION_RECORD_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MUTATION_OBSERVERS: RefCell<Vec<Rc<RefCell<MutationObserverState>>>> =
        const { RefCell::new(Vec::new()) };
    static INTERSECTION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static INTERSECTION_ENTRY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static INTERSECTION_OBSERVERS: RefCell<Vec<Rc<RefCell<IntersectionObserverState>>>> =
        const { RefCell::new(Vec::new()) };
    static INTERSECTION_VISIBILITY_WARNING: RefCell<bool> = const { RefCell::new(false) };
    static PERFORMANCE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERFORMANCE_ENTRY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERFORMANCE_MARK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERFORMANCE_MEASURE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERFORMANCE_LONG_TASK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TASK_ATTRIBUTION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static VISIBILITY_STATE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERFORMANCE_TIMELINE_CLASSES: RefCell<HashMap<String, Value>> =
        RefCell::new(HashMap::new());
    static NAVIGATION_DIAGNOSTIC_CLASSES: RefCell<HashMap<String, Value>> =
        RefCell::new(HashMap::new());
    static PERFORMANCE_ENTRY_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERFORMANCE_ENTRIES: RefCell<Vec<PerformanceEntry>> = const { RefCell::new(Vec::new()) };
    static PERFORMANCE_OBSERVERS: RefCell<Vec<Rc<RefCell<PerformanceObserverState>>>> =
        const { RefCell::new(Vec::new()) };
}

fn navigation_diagnostic_members(name: &str) -> &'static [&'static str] {
    match name {
        "NotRestoredReasonDetails" => &["reason"],
        "NotRestoredReasons" => &["children", "id", "name", "reasons", "src", "url"],
        _ => &[],
    }
}

pub fn navigation_diagnostic_class(name: &str) -> Value {
    NAVIGATION_DIAGNOSTIC_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let Some(name) = ["NotRestoredReasonDetails", "NotRestoredReasons"]
            .into_iter()
            .find(|candidate| candidate == &name)
        else {
            return Value::Undefined;
        };
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(&format!("{name} is not constructible"))],
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in navigation_diagnostic_members(name) {
            prototype.set_property(member, Value::Undefined);
        }
        prototype.set_property("toJSON", Value::Undefined);
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

/// Create a host-supplied BFCache restoration diagnostic with browser identity.
pub fn navigation_diagnostic_value(name: &str, values: Value) -> Value {
    let diagnostic = Value::object(HashMap::new());
    for member in navigation_diagnostic_members(name) {
        let supplied = values.get_property(member);
        let value = if !supplied.is_undefined() {
            supplied
        } else if matches!(*member, "children" | "reasons") {
            Value::array(Vec::new())
        } else {
            Value::string("")
        };
        diagnostic.set_property(member, value);
    }
    let diagnostic_for_json = diagnostic.clone();
    let class_name = name.to_string();
    diagnostic.set_property(
        "toJSON",
        Value::function(move |_, _| {
            let mut snapshot = HashMap::new();
            for member in navigation_diagnostic_members(&class_name) {
                snapshot.insert(
                    (*member).to_string(),
                    diagnostic_for_json.get_property(member),
                );
            }
            Value::object(snapshot)
        }),
    );
    w3cos_core::class::set_prototype_of(
        &diagnostic,
        &navigation_diagnostic_class(name).get_property("prototype"),
    );
    diagnostic
}

#[derive(Clone)]
struct PerformanceEntry {
    name: String,
    entry_type: String,
    start_time: f64,
    duration: f64,
    detail: Value,
}

const PERFORMANCE_TIMELINE_ENTRY_TYPES: &[&str] = &[
    "element",
    "event",
    "largest-contentful-paint",
    "layout-shift",
    "long-animation-frame",
    "navigation",
    "paint",
    "resource",
    "script",
];

fn performance_timeline_spec(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match name {
        "LargestContentfulPaint" => (
            "PerformanceEntry",
            &[
                "element",
                "id",
                "loadTime",
                "paintTime",
                "presentationTime",
                "renderTime",
                "size",
                "toJSON",
                "url",
            ],
        ),
        "LayoutShift" => (
            "PerformanceEntry",
            &[
                "hadRecentInput",
                "lastInputTime",
                "sources",
                "toJSON",
                "value",
            ],
        ),
        "LayoutShiftAttribution" => ("", &["currentRect", "node", "previousRect", "toJSON"]),
        "PerformanceElementTiming" => (
            "PerformanceEntry",
            &[
                "element",
                "id",
                "identifier",
                "intersectionRect",
                "loadTime",
                "naturalHeight",
                "naturalWidth",
                "paintTime",
                "presentationTime",
                "renderTime",
                "toJSON",
                "url",
            ],
        ),
        "PerformanceEventTiming" => (
            "PerformanceEntry",
            &[
                "cancelable",
                "interactionId",
                "processingEnd",
                "processingStart",
                "target",
                "toJSON",
            ],
        ),
        "PerformanceLongAnimationFrameTiming" => (
            "PerformanceEntry",
            &[
                "blockingDuration",
                "firstUIEventTimestamp",
                "paintTime",
                "presentationTime",
                "renderStart",
                "scripts",
                "styleAndLayoutStart",
                "toJSON",
            ],
        ),
        "PerformanceNavigationTiming" => (
            "PerformanceResourceTiming",
            &[
                "activationStart",
                "confidence",
                "criticalCHRestart",
                "domComplete",
                "domContentLoadedEventEnd",
                "domContentLoadedEventStart",
                "domInteractive",
                "loadEventEnd",
                "loadEventStart",
                "notRestoredReasons",
                "redirectCount",
                "toJSON",
                "type",
                "unloadEventEnd",
                "unloadEventStart",
            ],
        ),
        "PerformancePaintTiming" => (
            "PerformanceEntry",
            &["paintTime", "presentationTime", "toJSON"],
        ),
        "PerformanceResourceTiming" => (
            "PerformanceEntry",
            &[
                "connectEnd",
                "connectStart",
                "contentEncoding",
                "contentType",
                "decodedBodySize",
                "deliveryType",
                "domainLookupEnd",
                "domainLookupStart",
                "encodedBodySize",
                "fetchStart",
                "finalResponseHeadersStart",
                "firstInterimResponseStart",
                "initiatorType",
                "nextHopProtocol",
                "redirectEnd",
                "redirectStart",
                "renderBlockingStatus",
                "requestStart",
                "responseEnd",
                "responseStart",
                "responseStatus",
                "secureConnectionStart",
                "serverTiming",
                "toJSON",
                "transferSize",
                "workerCacheLookupStart",
                "workerFinalSourceType",
                "workerMatchedSourceType",
                "workerRouterEvaluationStart",
                "workerStart",
            ],
        ),
        "PerformanceScriptTiming" => (
            "PerformanceEntry",
            &[
                "executionStart",
                "forcedStyleAndLayoutDuration",
                "invoker",
                "invokerType",
                "pauseDuration",
                "sourceCharPosition",
                "sourceFunctionName",
                "sourceURL",
                "toJSON",
                "window",
                "windowAttribution",
            ],
        ),
        "PerformanceServerTiming" => ("", &["description", "duration", "name", "toJSON"]),
        "PerformanceTimingConfidence" => ("", &["randomizedTriggerRate", "toJSON", "value"]),
        _ => return None,
    })
}

fn performance_class_for_entry_type(entry_type: &str) -> Option<&'static str> {
    Some(match entry_type {
        "element" => "PerformanceElementTiming",
        "event" => "PerformanceEventTiming",
        "largest-contentful-paint" => "LargestContentfulPaint",
        "layout-shift" => "LayoutShift",
        "long-animation-frame" => "PerformanceLongAnimationFrameTiming",
        "navigation" => "PerformanceNavigationTiming",
        "paint" => "PerformancePaintTiming",
        "resource" => "PerformanceResourceTiming",
        "script" => "PerformanceScriptTiming",
        _ => return None,
    })
}

#[derive(Clone, Debug, Default)]
pub struct TaskAttribution {
    pub name: String,
    pub container_type: String,
    pub container_src: String,
    pub container_id: String,
    pub container_name: String,
}

struct PerformanceObserverState {
    callback: Value,
    observer: Value,
    entry_types: HashSet<String>,
    pending: Vec<Value>,
    active: bool,
    scheduled: bool,
}

#[derive(Clone)]
struct MutationObservation {
    target: u32,
    child_list: bool,
    attributes: bool,
    character_data: bool,
    subtree: bool,
    attribute_old_value: bool,
    character_data_old_value: bool,
    attribute_filter: Option<HashSet<String>>,
}

struct MutationObserverState {
    callback: Value,
    observer: Value,
    observations: Vec<MutationObservation>,
    pending: Vec<Value>,
    scheduled: bool,
}

struct DomResizeTarget {
    box_kind: String,
    last_size: Option<(f64, f64)>,
}

struct DomResizeObserverState {
    callback: Value,
    observer: Value,
    targets: HashMap<u32, DomResizeTarget>,
}

#[derive(Clone, Copy)]
enum RootMargin {
    Px(f64),
    Percent(f64),
}

struct IntersectionObserverState {
    callback: Value,
    observer: Value,
    root: Option<u32>,
    margins: [RootMargin; 4],
    thresholds: Vec<f64>,
    track_visibility: bool,
    targets: HashMap<u32, Option<(f64, bool)>>,
    pending: Vec<Value>,
    scheduled: bool,
}

fn finish_class(class: Value) -> Value {
    finish_class_with_members(class, "", &[], &[])
}

fn finish_class_with_members(
    class: Value,
    name: &'static str,
    methods: &[&str],
    properties: &[&str],
) -> Value {
    if !name.is_empty() {
        class.set_property("name", Value::string(name));
    }
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for method in methods {
        prototype.set_property(method, Value::function(|_, _| Value::Undefined));
    }
    for property in properties {
        prototype.set_property(property, Value::Undefined);
    }
    class.set_property("prototype", prototype);
    class
}

fn illegal_observer_record_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    properties: &'static [&'static str],
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(&format!("Illegal constructor: {name}"))],
            ))
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

pub fn resize_observer_entry_class() -> Value {
    illegal_observer_record_class(
        &RESIZE_ENTRY_CLASS,
        "ResizeObserverEntry",
        &[
            "borderBoxSize",
            "contentBoxSize",
            "contentRect",
            "devicePixelContentBoxSize",
            "target",
        ],
    )
}

pub fn resize_observer_size_class() -> Value {
    illegal_observer_record_class(
        &RESIZE_SIZE_CLASS,
        "ResizeObserverSize",
        &["blockSize", "inlineSize"],
    )
}

pub fn intersection_observer_entry_class() -> Value {
    illegal_observer_record_class(
        &INTERSECTION_ENTRY_CLASS,
        "IntersectionObserverEntry",
        &[
            "boundingClientRect",
            "intersectionRatio",
            "intersectionRect",
            "isIntersecting",
            "isVisible",
            "rootBounds",
            "target",
            "time",
        ],
    )
}

pub fn mutation_record_class() -> Value {
    illegal_observer_record_class(
        &MUTATION_RECORD_CLASS,
        "MutationRecord",
        &[
            "addedNodes",
            "attributeName",
            "attributeNamespace",
            "nextSibling",
            "oldValue",
            "previousSibling",
            "removedNodes",
            "target",
            "type",
        ],
    )
}

pub fn resize_observer_class() -> Value {
    RESIZE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class_with_members(
            Value::function(|_, args| {
                let callback = args.first().cloned().unwrap_or_default();
                if !callback.is_function() {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string("ResizeObserver callback must be a function")],
                    ));
                }
                let observer = w3cos_core::ResizeObserver::new(args);
                let state = Rc::new(RefCell::new(DomResizeObserverState {
                    callback,
                    observer: observer.clone(),
                    targets: HashMap::new(),
                }));
                DOM_RESIZE_OBSERVERS
                    .with(|observers| observers.borrow_mut().push(Rc::clone(&state)));
                let original_observe = observer.get_property("observe");
                let observe_state = Rc::clone(&state);
                observer.set_property(
                    "observe",
                    Value::function(move |this, args| {
                        let target_value = args.first().cloned().unwrap_or_default();
                        let options = args.get(1).cloned().unwrap_or_default();
                        let box_kind = if options.get_property("box").is_undefined() {
                            "content-box".to_string()
                        } else {
                            options.get_property("box").to_js_string()
                        };
                        if !matches!(
                            box_kind.as_str(),
                            "content-box" | "border-box" | "device-pixel-content-box"
                        ) {
                            w3cos_core::throw_value(w3cos_core::error_instance(
                                "TypeError",
                                vec![Value::string("ResizeObserver box option is invalid")],
                            ));
                        }
                        let has_host_id = target_value
                            .get_property("__w3cosHostId")
                            .to_js_string()
                            .parse::<u64>()
                            .is_ok();
                        original_observe.call(this, args.clone());
                        if has_host_id {
                            if box_kind == "device-pixel-content-box" {
                                RESIZE_DEVICE_PIXEL_HOST_WARNING.with(|warned| {
                                    if !*warned.borrow() {
                                        *warned.borrow_mut() = true;
                                        eprintln!(
                                            "[W3C OS][compat warning] ResizeObserver \
                                         device-pixel-content-box on host-backed targets uses \
                                         CSS-pixel host measurements because host DPR metadata \
                                         is unavailable"
                                        );
                                    }
                                });
                            }
                            return Value::Undefined;
                        }
                        let Some(target) = crate::jsdom::node_id_of(&target_value) else {
                            w3cos_core::throw_value(w3cos_core::error_instance(
                                "TypeError",
                                vec![Value::string(
                                    "ResizeObserver.observe target must be an Element",
                                )],
                            ));
                        };
                        observe_state.borrow_mut().targets.insert(
                            target,
                            DomResizeTarget {
                                box_kind,
                                last_size: None,
                            },
                        );
                        Value::Undefined
                    }),
                );
                let original_unobserve = observer.get_property("unobserve");
                let unobserve_state = Rc::clone(&state);
                observer.set_property(
                    "unobserve",
                    Value::function(move |this, args| {
                        original_unobserve.call(this, args.clone());
                        if let Some(target) = args.first().and_then(crate::jsdom::node_id_of) {
                            unobserve_state.borrow_mut().targets.remove(&target);
                        }
                        Value::Undefined
                    }),
                );
                let original_disconnect = observer.get_property("disconnect");
                let disconnect_state = state;
                observer.set_property(
                    "disconnect",
                    Value::function(move |this, args| {
                        original_disconnect.call(this, args);
                        disconnect_state.borrow_mut().targets.clear();
                        Value::Undefined
                    }),
                );
                w3cos_core::class::set_prototype_of(
                    &observer,
                    &resize_observer_class().get_property("prototype"),
                );
                observer
            }),
            "ResizeObserver",
            &["disconnect", "observe", "unobserve"],
            &[],
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn css_pixels(node: u32, property: &str) -> f64 {
    crate::jsdom::resolved_style_property(node, property)
        .trim()
        .strip_suffix("px")
        .unwrap_or("0")
        .parse()
        .unwrap_or(0.0)
}

fn resize_box_size(inline_size: f64, block_size: f64) -> Value {
    let value = Value::object(HashMap::from([
        ("inlineSize".to_string(), Value::Number(inline_size)),
        ("blockSize".to_string(), Value::Number(block_size)),
    ]));
    w3cos_core::class::set_prototype_of(
        &value,
        &resize_observer_size_class().get_property("prototype"),
    );
    value
}

fn dom_resize_geometry(node: u32) -> (w3cos_dom::DOMRect, f64, f64) {
    let border = crate::dom::bounding_rect(node);
    let horizontal = css_pixels(node, "padding-left")
        + css_pixels(node, "padding-right")
        + css_pixels(node, "border-left-width")
        + css_pixels(node, "border-right-width");
    let vertical = css_pixels(node, "padding-top")
        + css_pixels(node, "padding-bottom")
        + css_pixels(node, "border-top-width")
        + css_pixels(node, "border-bottom-width");
    (
        border,
        (border.width as f64 - horizontal).max(0.0),
        (border.height as f64 - vertical).max(0.0),
    )
}

pub fn refresh_resize_observers() {
    let dpr = crate::jsdom::viewport().2;
    let deliveries = DOM_RESIZE_OBSERVERS.with(|observers| {
        let mut deliveries = Vec::new();
        for observer in observers.borrow().iter() {
            let mut state = observer.borrow_mut();
            let mut entries = Vec::new();
            let targets = state.targets.keys().copied().collect::<Vec<_>>();
            for target in targets {
                let (border, content_width, content_height) = dom_resize_geometry(target);
                let Some(observed) = state.targets.get_mut(&target) else {
                    continue;
                };
                let selected = match observed.box_kind.as_str() {
                    "border-box" => (border.width as f64, border.height as f64),
                    "device-pixel-content-box" => (content_width * dpr, content_height * dpr),
                    _ => (content_width, content_height),
                };
                if observed.last_size.is_some_and(|previous| {
                    (previous.0 - selected.0).abs() <= 0.01
                        && (previous.1 - selected.1).abs() <= 0.01
                }) {
                    continue;
                }
                observed.last_size = Some(selected);
                let content_rect =
                    w3cos_dom::DOMRect::new(0.0, 0.0, content_width as f32, content_height as f32);
                let entry = Value::object(HashMap::from([
                    ("target".to_string(), crate::jsdom::element_value(target)),
                    (
                        "contentRect".to_string(),
                        intersection_rect_value(content_rect),
                    ),
                    (
                        "borderBoxSize".to_string(),
                        Value::array(vec![resize_box_size(
                            border.width as f64,
                            border.height as f64,
                        )]),
                    ),
                    (
                        "contentBoxSize".to_string(),
                        Value::array(vec![resize_box_size(content_width, content_height)]),
                    ),
                    (
                        "devicePixelContentBoxSize".to_string(),
                        Value::array(vec![resize_box_size(
                            content_width * dpr,
                            content_height * dpr,
                        )]),
                    ),
                ]));
                w3cos_core::class::set_prototype_of(
                    &entry,
                    &resize_observer_entry_class().get_property("prototype"),
                );
                entries.push(entry);
            }
            if !entries.is_empty() {
                deliveries.push((
                    state.callback.clone(),
                    state.observer.clone(),
                    Value::array(entries),
                ));
            }
        }
        deliveries
    });
    for (callback, observer, entries) in deliveries {
        callback.call(Value::Undefined, vec![entries, observer]);
    }
}

pub fn mutation_observer_class() -> Value {
    MUTATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class_with_members(Value::function(|_, args| {
            let callback = args.first().cloned().unwrap_or_default();
            if !callback.is_function() {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string(
                        "PerformanceObserver callback must be a function",
                    )],
                ));
            }
            let value = Value::object(HashMap::new());
            let state = Rc::new(RefCell::new(MutationObserverState {
                callback,
                observer: value.clone(),
                observations: Vec::new(),
                pending: Vec::new(),
                scheduled: false,
            }));
            MUTATION_OBSERVERS.with(|observers| {
                observers.borrow_mut().push(Rc::clone(&state));
            });
            let observe_state = Rc::clone(&state);
            value.set_property(
                "observe",
                Value::function(move |_, args| {
                    let Some(target) = args
                        .first()
                        .and_then(crate::jsdom::node_id_of)
                    else {
                        w3cos_core::throw_value(Value::object(HashMap::from([
                            ("name".to_string(), Value::string("TypeError")),
                            (
                                "message".to_string(),
                                Value::string("MutationObserver.observe target must be a Node"),
                            ),
                        ])));
                    };
                    let options = args.get(1).cloned().unwrap_or_default();
                    let attribute_filter = options.get_property("attributeFilter");
                    let attribute_old_value = options.get_property("attributeOldValue").to_bool();
                    let character_data_old_value =
                        options.get_property("characterDataOldValue").to_bool();
                    let attributes = options.get_property("attributes").to_bool()
                        || attribute_old_value
                        || !attribute_filter.is_undefined();
                    let character_data = options.get_property("characterData").to_bool()
                        || character_data_old_value;
                    let child_list = options.get_property("childList").to_bool();
                    if !attributes && !character_data && !child_list {
                        w3cos_core::throw_value(Value::object(HashMap::from([
                            ("name".to_string(), Value::string("TypeError")),
                            (
                                "message".to_string(),
                                Value::string(
                                    "MutationObserver options must enable at least one mutation type",
                                ),
                            ),
                        ])));
                    }
                    let observation = MutationObservation {
                        target,
                        child_list,
                        attributes,
                        character_data,
                        subtree: options.get_property("subtree").to_bool(),
                        attribute_old_value,
                        character_data_old_value,
                        attribute_filter: (!attribute_filter.is_undefined()).then(|| {
                            attribute_filter
                                .iter()
                                .map(|value| value.to_js_string())
                                .collect()
                        }),
                    };
                    let mut state = observe_state.borrow_mut();
                    if let Some(existing) = state
                        .observations
                        .iter_mut()
                        .find(|existing| existing.target == target)
                    {
                        *existing = observation;
                    } else {
                        state.observations.push(observation);
                    }
                    Value::Undefined
                }),
            );
            let disconnect_state = Rc::clone(&state);
            value.set_property(
                "disconnect",
                Value::function(move |_, _| {
                    let mut state = disconnect_state.borrow_mut();
                    state.observations.clear();
                    state.pending.clear();
                    Value::Undefined
                }),
            );
            let take_state = Rc::clone(&state);
            value.set_property(
                "takeRecords",
                Value::function(move |_, _| {
                    Value::array(std::mem::take(&mut take_state.borrow_mut().pending))
                }),
            );
            let enqueue_state = state;
            value.set_property(
                "__w3cosEnqueue",
                Value::function(move |_, args| {
                    let record = args.first().cloned().unwrap_or_default();
                    enqueue_state.borrow_mut().pending.push(record);
                    schedule_mutation_delivery(&enqueue_state);
                    Value::Undefined
                }),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &mutation_observer_class().get_property("prototype"),
            );
            value
        }), "MutationObserver", &["disconnect", "observe", "takeRecords"], &[]);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn mutation_target_matches(observation: &MutationObservation, target: u32) -> bool {
    if observation.target == target {
        return true;
    }
    if !observation.subtree {
        return false;
    }
    let mut parent = crate::dom::parent_node(target);
    while let Some(node) = parent {
        if node == observation.target {
            return true;
        }
        parent = crate::dom::parent_node(node);
    }
    false
}

fn schedule_mutation_delivery(state: &Rc<RefCell<MutationObserverState>>) {
    if state.borrow().scheduled {
        return;
    }
    state.borrow_mut().scheduled = true;
    let delivery = Rc::clone(state);
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        let (callback, observer, records) = {
            let mut state = delivery.borrow_mut();
            state.scheduled = false;
            if state.pending.is_empty() {
                return Value::Undefined;
            }
            (
                state.callback.clone(),
                state.observer.clone(),
                std::mem::take(&mut state.pending),
            )
        };
        callback.call(Value::Undefined, vec![Value::array(records), observer]);
        Value::Undefined
    }));
}

fn mutation_record(
    mutation_type: &str,
    target: u32,
    added: &[u32],
    removed: &[u32],
    previous_sibling: Option<u32>,
    next_sibling: Option<u32>,
    attribute_name: Option<&str>,
    old_value: Option<&str>,
) -> Value {
    let value = Value::object(HashMap::from([
        ("type".to_string(), Value::string(mutation_type)),
        ("target".to_string(), crate::jsdom::element_value(target)),
        (
            "addedNodes".to_string(),
            crate::jsdom::node_list(
                added
                    .iter()
                    .copied()
                    .map(crate::jsdom::element_value)
                    .collect(),
            ),
        ),
        (
            "removedNodes".to_string(),
            crate::jsdom::node_list(
                removed
                    .iter()
                    .copied()
                    .map(crate::jsdom::element_value)
                    .collect(),
            ),
        ),
        (
            "previousSibling".to_string(),
            previous_sibling
                .map(crate::jsdom::element_value)
                .unwrap_or(Value::Null),
        ),
        (
            "nextSibling".to_string(),
            next_sibling
                .map(crate::jsdom::element_value)
                .unwrap_or(Value::Null),
        ),
        (
            "attributeName".to_string(),
            attribute_name.map(Value::string).unwrap_or(Value::Null),
        ),
        ("attributeNamespace".to_string(), Value::Null),
        (
            "oldValue".to_string(),
            old_value.map(Value::string).unwrap_or(Value::Null),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &mutation_record_class().get_property("prototype"));
    value
}

pub fn notify_child_list(
    target: u32,
    added: &[u32],
    removed: &[u32],
    previous_sibling: Option<u32>,
    next_sibling: Option<u32>,
) {
    MUTATION_OBSERVERS.with(|observers| {
        for state in observers.borrow().iter() {
            let matches = state.borrow().observations.iter().any(|observation| {
                observation.child_list && mutation_target_matches(observation, target)
            });
            if matches {
                state.borrow_mut().pending.push(mutation_record(
                    "childList",
                    target,
                    added,
                    removed,
                    previous_sibling,
                    next_sibling,
                    None,
                    None,
                ));
                schedule_mutation_delivery(state);
            }
        }
    });
}

pub fn notify_attribute(target: u32, name: &str, old_value: Option<&str>) {
    MUTATION_OBSERVERS.with(|observers| {
        for state in observers.borrow().iter() {
            let include_old_value = state
                .borrow()
                .observations
                .iter()
                .filter(|observation| {
                    observation.attributes
                        && mutation_target_matches(observation, target)
                        && observation
                            .attribute_filter
                            .as_ref()
                            .is_none_or(|filter| filter.contains(name))
                })
                .map(|observation| observation.attribute_old_value)
                .reduce(|left, right| left || right);
            if let Some(include_old_value) = include_old_value {
                state.borrow_mut().pending.push(mutation_record(
                    "attributes",
                    target,
                    &[],
                    &[],
                    None,
                    None,
                    Some(name),
                    include_old_value.then_some(old_value).flatten(),
                ));
                schedule_mutation_delivery(state);
            }
        }
    });
}

pub fn notify_character_data(target: u32, old_value: Option<&str>) {
    MUTATION_OBSERVERS.with(|observers| {
        for state in observers.borrow().iter() {
            let include_old_value = state
                .borrow()
                .observations
                .iter()
                .filter(|observation| {
                    observation.character_data && mutation_target_matches(observation, target)
                })
                .map(|observation| observation.character_data_old_value)
                .reduce(|left, right| left || right);
            if let Some(include_old_value) = include_old_value {
                state.borrow_mut().pending.push(mutation_record(
                    "characterData",
                    target,
                    &[],
                    &[],
                    None,
                    None,
                    None,
                    include_old_value.then_some(old_value).flatten(),
                ));
                schedule_mutation_delivery(state);
            }
        }
    });
}

fn parse_root_margin(input: &str) -> Result<([RootMargin; 4], String), String> {
    let tokens = input.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() || tokens.len() > 4 {
        return Err("rootMargin must contain one to four lengths".to_string());
    }
    let mut parsed = Vec::new();
    for token in tokens {
        let margin = if let Some(value) = token.strip_suffix("px") {
            RootMargin::Px(
                value
                    .parse()
                    .map_err(|_| format!("invalid rootMargin component '{token}'"))?,
            )
        } else if let Some(value) = token.strip_suffix('%') {
            RootMargin::Percent(
                value
                    .parse()
                    .map_err(|_| format!("invalid rootMargin component '{token}'"))?,
            )
        } else {
            return Err(format!(
                "unsupported rootMargin component '{token}'; expected px or %"
            ));
        };
        parsed.push((margin, token.to_string()));
    }
    let expanded = match parsed.as_slice() {
        [a] => [a.clone(), a.clone(), a.clone(), a.clone()],
        [vertical, horizontal] => [
            vertical.clone(),
            horizontal.clone(),
            vertical.clone(),
            horizontal.clone(),
        ],
        [top, horizontal, bottom] => [
            top.clone(),
            horizontal.clone(),
            bottom.clone(),
            horizontal.clone(),
        ],
        [top, right, bottom, left] => [top.clone(), right.clone(), bottom.clone(), left.clone()],
        _ => unreachable!(),
    };
    Ok((
        [expanded[0].0, expanded[1].0, expanded[2].0, expanded[3].0],
        expanded
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    ))
}

fn intersection_rect_value(rect: w3cos_dom::DOMRect) -> Value {
    let value = Value::object(HashMap::from([
        ("x".to_string(), Value::Number(rect.x as f64)),
        ("y".to_string(), Value::Number(rect.y as f64)),
        ("width".to_string(), Value::Number(rect.width as f64)),
        ("height".to_string(), Value::Number(rect.height as f64)),
        ("top".to_string(), Value::Number(rect.y as f64)),
        ("left".to_string(), Value::Number(rect.x as f64)),
        (
            "right".to_string(),
            Value::Number((rect.x + rect.width) as f64),
        ),
        (
            "bottom".to_string(),
            Value::Number((rect.y + rect.height) as f64),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::geometry_web::class("DOMRectReadOnly").get_property("prototype"),
    );
    value
}

fn resolve_margin(margin: RootMargin, reference: f64) -> f64 {
    match margin {
        RootMargin::Px(value) => value,
        RootMargin::Percent(value) => reference * value / 100.0,
    }
}

fn intersection_geometry(
    state: &IntersectionObserverState,
    target: u32,
) -> (
    w3cos_dom::DOMRect,
    w3cos_dom::DOMRect,
    w3cos_dom::DOMRect,
    f64,
    bool,
) {
    let target_rect = crate::dom::bounding_rect(target);
    let mut root_rect = state
        .root
        .map(crate::dom::bounding_rect)
        .unwrap_or_else(|| {
            let (width, height, _) = crate::jsdom::viewport();
            w3cos_dom::DOMRect::new(0.0, 0.0, width as f32, height as f32)
        });
    let top = resolve_margin(state.margins[0], root_rect.height as f64) as f32;
    let right = resolve_margin(state.margins[1], root_rect.width as f64) as f32;
    let bottom = resolve_margin(state.margins[2], root_rect.height as f64) as f32;
    let left = resolve_margin(state.margins[3], root_rect.width as f64) as f32;
    root_rect = w3cos_dom::DOMRect::new(
        root_rect.x - left,
        root_rect.y - top,
        (root_rect.width + left + right).max(0.0),
        (root_rect.height + top + bottom).max(0.0),
    );
    let x1 = target_rect.x.max(root_rect.x);
    let y1 = target_rect.y.max(root_rect.y);
    let x2 = (target_rect.x + target_rect.width).min(root_rect.x + root_rect.width);
    let y2 = (target_rect.y + target_rect.height).min(root_rect.y + root_rect.height);
    let intersection = if x2 > x1 && y2 > y1 {
        w3cos_dom::DOMRect::new(x1, y1, x2 - x1, y2 - y1)
    } else {
        w3cos_dom::DOMRect::zero()
    };
    let target_area = target_rect.width.max(0.0) as f64 * target_rect.height.max(0.0) as f64;
    let intersection_area =
        intersection.width.max(0.0) as f64 * intersection.height.max(0.0) as f64;
    let is_intersecting = intersection_area > 0.0;
    let ratio = if target_area > 0.0 {
        (intersection_area / target_area).clamp(0.0, 1.0)
    } else if is_intersecting {
        1.0
    } else {
        0.0
    };
    (target_rect, root_rect, intersection, ratio, is_intersecting)
}

fn schedule_intersection_delivery(state: &Rc<RefCell<IntersectionObserverState>>) {
    if state.borrow().scheduled || state.borrow().pending.is_empty() {
        return;
    }
    state.borrow_mut().scheduled = true;
    let delivery = Rc::clone(state);
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        let (callback, observer, records) = {
            let mut state = delivery.borrow_mut();
            state.scheduled = false;
            if state.pending.is_empty() {
                return Value::Undefined;
            }
            (
                state.callback.clone(),
                state.observer.clone(),
                std::mem::take(&mut state.pending),
            )
        };
        callback.call(Value::Undefined, vec![Value::array(records), observer]);
        Value::Undefined
    }));
}

fn refresh_intersection_observer(state: &Rc<RefCell<IntersectionObserverState>>) {
    let targets = state.borrow().targets.keys().copied().collect::<Vec<_>>();
    for target in targets {
        let (target_rect, root_rect, intersection, ratio, is_intersecting) = {
            let state = state.borrow();
            intersection_geometry(&state, target)
        };
        let should_queue = {
            let state = state.borrow();
            match state.targets.get(&target).copied().flatten() {
                None => true,
                Some((previous_ratio, previous_intersecting)) => {
                    previous_intersecting != is_intersecting
                        || state.thresholds.iter().any(|threshold| {
                            (previous_ratio < *threshold && ratio >= *threshold)
                                || (previous_ratio >= *threshold && ratio < *threshold)
                        })
                }
            }
        };
        if !should_queue {
            continue;
        }
        let mut state = state.borrow_mut();
        let is_visible = !state.track_visibility || is_intersecting;
        state.targets.insert(target, Some((ratio, is_intersecting)));
        let entry = Value::object(HashMap::from([
            ("target".to_string(), crate::jsdom::element_value(target)),
            (
                "time".to_string(),
                Value::Number(crate::jsdom::performance_now()),
            ),
            ("rootBounds".to_string(), intersection_rect_value(root_rect)),
            (
                "boundingClientRect".to_string(),
                intersection_rect_value(target_rect),
            ),
            (
                "intersectionRect".to_string(),
                intersection_rect_value(intersection),
            ),
            ("intersectionRatio".to_string(), Value::Number(ratio)),
            ("isIntersecting".to_string(), Value::Bool(is_intersecting)),
            ("isVisible".to_string(), Value::Bool(is_visible)),
        ]));
        w3cos_core::class::set_prototype_of(
            &entry,
            &intersection_observer_entry_class().get_property("prototype"),
        );
        state.pending.push(entry);
    }
    schedule_intersection_delivery(state);
}

pub fn refresh_intersection_observers() {
    INTERSECTION_OBSERVERS.with(|observers| {
        for observer in observers.borrow().iter() {
            refresh_intersection_observer(observer);
        }
    });
}

pub fn intersection_observer_class() -> Value {
    INTERSECTION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class_with_members(
            Value::function(|_, args| {
                let callback = args.first().cloned().unwrap_or_default();
                if !callback.is_function() {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string(
                            "IntersectionObserver callback must be a function",
                        )],
                    ));
                }
                let options = args.get(1).cloned().unwrap_or_default();
                let root_value = options.get_property("root");
                let root = if root_value.is_nullish() {
                    None
                } else if let Some(root) = crate::jsdom::node_id_of(&root_value) {
                    Some(root)
                } else {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string(
                            "IntersectionObserver root must be an Element or null",
                        )],
                    ));
                };
                let root_property = if root.is_some() {
                    root_value
                } else {
                    Value::Null
                };
                let threshold = options.get_property("threshold");
                let mut thresholds = if threshold.is_undefined() {
                    vec![0.0]
                } else if matches!(threshold, Value::Array(_)) {
                    threshold.iter().map(|value| value.to_number()).collect()
                } else {
                    vec![threshold.to_number()]
                };
                if thresholds
                    .iter()
                    .any(|threshold| !threshold.is_finite() || !(0.0..=1.0).contains(threshold))
                {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "RangeError",
                        vec![Value::string(
                            "IntersectionObserver thresholds must be between 0 and 1",
                        )],
                    ));
                }
                thresholds.sort_by(f64::total_cmp);
                thresholds.dedup_by(|left, right| left == right);
                if thresholds.is_empty() {
                    thresholds.push(0.0);
                }
                let root_margin_input = if options.get_property("rootMargin").is_undefined() {
                    "0px".to_string()
                } else {
                    options.get_property("rootMargin").to_js_string()
                };
                let (margins, root_margin) =
                    parse_root_margin(&root_margin_input).unwrap_or_else(|message| {
                        w3cos_core::throw_value(w3cos_core::error_instance(
                            "SyntaxError",
                            vec![Value::string(&message)],
                        ))
                    });
                let track_visibility = options.get_property("trackVisibility").to_bool();
                let delay = options.get_property("delay");
                let delay = if delay.is_undefined() {
                    0.0
                } else {
                    delay.to_number().max(0.0)
                };
                if track_visibility {
                    INTERSECTION_VISIBILITY_WARNING.with(|warned| {
                        if !std::mem::replace(&mut *warned.borrow_mut(), true) {
                            eprintln!(
                                "[W3C OS][compat warning] IntersectionObserver trackVisibility \
                             reports geometric visibility; occlusion tracking is unavailable"
                            );
                        }
                    });
                }
                let value = Value::object(HashMap::from([
                    ("root".to_string(), root_property),
                    ("rootMargin".to_string(), Value::string(&root_margin)),
                    ("scrollMargin".to_string(), Value::string("0px 0px 0px 0px")),
                    (
                        "thresholds".to_string(),
                        Value::array(thresholds.iter().copied().map(Value::Number).collect()),
                    ),
                    ("trackVisibility".to_string(), Value::Bool(track_visibility)),
                    ("delay".to_string(), Value::Number(delay)),
                ]));
                let state = Rc::new(RefCell::new(IntersectionObserverState {
                    callback,
                    observer: value.clone(),
                    root,
                    margins,
                    thresholds,
                    track_visibility,
                    targets: HashMap::new(),
                    pending: Vec::new(),
                    scheduled: false,
                }));
                INTERSECTION_OBSERVERS
                    .with(|observers| observers.borrow_mut().push(Rc::clone(&state)));
                let observe_state = Rc::clone(&state);
                value.set_property(
                    "observe",
                    Value::function(move |_, args| {
                        let Some(target) = args.first().and_then(crate::jsdom::node_id_of) else {
                            w3cos_core::throw_value(w3cos_core::error_instance(
                                "TypeError",
                                vec![Value::string(
                                    "IntersectionObserver.observe target must be an Element",
                                )],
                            ));
                        };
                        if observe_state.borrow().targets.contains_key(&target) {
                            return Value::Undefined;
                        }
                        observe_state.borrow_mut().targets.insert(target, None);
                        refresh_intersection_observer(&observe_state);
                        Value::Undefined
                    }),
                );
                let unobserve_state = Rc::clone(&state);
                value.set_property(
                    "unobserve",
                    Value::function(move |_, args| {
                        if let Some(target) = args.first().and_then(crate::jsdom::node_id_of) {
                            unobserve_state.borrow_mut().targets.remove(&target);
                        }
                        Value::Undefined
                    }),
                );
                let disconnect_state = Rc::clone(&state);
                value.set_property(
                    "disconnect",
                    Value::function(move |_, _| {
                        let mut state = disconnect_state.borrow_mut();
                        state.targets.clear();
                        state.pending.clear();
                        Value::Undefined
                    }),
                );
                let records_state = state;
                value.set_property(
                    "takeRecords",
                    Value::function(move |_, _| {
                        Value::array(std::mem::take(&mut records_state.borrow_mut().pending))
                    }),
                );
                w3cos_core::class::set_prototype_of(
                    &value,
                    &intersection_observer_class().get_property("prototype"),
                );
                value
            }),
            "IntersectionObserver",
            &["disconnect", "observe", "takeRecords", "unobserve"],
            &[
                "delay",
                "root",
                "rootMargin",
                "scrollMargin",
                "thresholds",
                "trackVisibility",
            ],
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn performance_observer_class() -> Value {
    PERFORMANCE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class_with_members(
            Value::function(|_, args| {
                let callback = args.first().cloned().unwrap_or_default();
                let value = Value::object(HashMap::new());
                let state = Rc::new(RefCell::new(PerformanceObserverState {
                    callback,
                    observer: value.clone(),
                    entry_types: HashSet::new(),
                    pending: Vec::new(),
                    active: false,
                    scheduled: false,
                }));
                PERFORMANCE_OBSERVERS.with(|observers| {
                    observers.borrow_mut().push(Rc::clone(&state));
                });
                let observe_state = Rc::clone(&state);
                value.set_property(
                    "observe",
                    Value::function(move |_, args| {
                        let options = args.first().cloned().unwrap_or_default();
                        let entry_types = options.get_property("entryTypes");
                        let single_type = options.get_property("type");
                        let mut types = HashSet::new();
                        if !entry_types.is_undefined() {
                            types.extend(entry_types.iter().map(|value| value.to_js_string()));
                        } else if !single_type.is_undefined() {
                            types.insert(single_type.to_js_string());
                        }
                        types.retain(|kind| {
                            matches!(
                                kind.as_str(),
                                "mark" | "measure" | "longtask" | "visibility-state"
                            ) || PERFORMANCE_TIMELINE_ENTRY_TYPES.contains(&kind.as_str())
                        });
                        if types.iter().any(|kind| {
                            kind == "longtask"
                                || PERFORMANCE_TIMELINE_ENTRY_TYPES.contains(&kind.as_str())
                        }) {
                            static WARNING: Once = Once::new();
                            WARNING.call_once(|| {
                                eprintln!(
                                    "[w3cos] warning: automatic PerformanceObserver delivery for \
                                 rendering, resource and long-task entries requires host \
                                 instrumentation; records remain available through the runtime \
                                 injection boundary"
                                );
                            });
                        }
                        {
                            let mut state = observe_state.borrow_mut();
                            state.entry_types = types;
                            state.active = !state.entry_types.is_empty();
                        }
                        if options.get_property("buffered").to_bool() {
                            let buffered = PERFORMANCE_ENTRIES.with(|entries| {
                                let state = observe_state.borrow();
                                entries
                                    .borrow()
                                    .iter()
                                    .filter(|entry| state.entry_types.contains(&entry.entry_type))
                                    .map(performance_entry_value)
                                    .collect::<Vec<_>>()
                            });
                            if !buffered.is_empty() {
                                observe_state.borrow_mut().pending.extend(buffered);
                                schedule_performance_delivery(&observe_state);
                            }
                        }
                        Value::Undefined
                    }),
                );
                let disconnect_state = Rc::clone(&state);
                value.set_property(
                    "disconnect",
                    Value::function(move |_, _| {
                        let mut state = disconnect_state.borrow_mut();
                        state.active = false;
                        state.entry_types.clear();
                        state.pending.clear();
                        Value::Undefined
                    }),
                );
                let take_state = state;
                value.set_property(
                    "takeRecords",
                    Value::function(move |_, _| {
                        Value::array(std::mem::take(&mut take_state.borrow_mut().pending))
                    }),
                );
                w3cos_core::class::set_prototype_of(
                    &value,
                    &performance_observer_class().get_property("prototype"),
                );
                value
            }),
            "PerformanceObserver",
            &["disconnect", "observe", "takeRecords"],
            &[],
        );
        class.set_property(
            "supportedEntryTypes",
            Value::array(
                [
                    "element",
                    "event",
                    "largest-contentful-paint",
                    "layout-shift",
                    "long-animation-frame",
                    "longtask",
                    "mark",
                    "measure",
                    "navigation",
                    "paint",
                    "resource",
                    "script",
                    "visibility-state",
                ]
                .into_iter()
                .map(Value::string)
                .collect(),
            ),
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn performance_entry_class(name: &'static str) -> Value {
    let slot = match name {
        "PerformanceMark" => &PERFORMANCE_MARK_CLASS,
        "PerformanceMeasure" => &PERFORMANCE_MEASURE_CLASS,
        _ => &PERFORMANCE_ENTRY_CLASS,
    };
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = if name == "PerformanceMark" {
            finish_class(Value::function(|_, args| {
                if args.is_empty() {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string("PerformanceMark name is required")],
                    ));
                }
                let name = args.first().map(Value::to_js_string).unwrap_or_default();
                let options = args.get(1).cloned().unwrap_or_default();
                let start_time = options.get_property("startTime");
                let detail = options.get_property("detail");
                let start_time = if start_time.is_undefined() {
                    crate::jsdom::performance_now()
                } else {
                    let start_time = start_time.to_number();
                    if start_time < 0.0 {
                        w3cos_core::throw_value(w3cos_core::error_instance(
                            "TypeError",
                            vec![Value::string(
                                "PerformanceMark startTime must not be negative",
                            )],
                        ));
                    }
                    start_time
                };
                performance_entry_value(&PerformanceEntry {
                    name,
                    entry_type: "mark".to_string(),
                    start_time,
                    duration: 0.0,
                    detail: if detail.is_undefined() {
                        Value::Null
                    } else {
                        w3cos_core::web::structured_clone(vec![detail])
                    },
                })
            }))
        } else {
            finish_class(Value::function(move |_, _| {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string(&format!("{name} is not constructible"))],
                ))
            }))
        };
        if name == "PerformanceEntry" {
            let prototype = class.get_property("prototype");
            for member in ["name", "entryType", "startTime", "duration", "toJSON"] {
                prototype.set_property(member, Value::Undefined);
            }
        }
        if name != "PerformanceEntry" {
            w3cos_core::class::set_prototype_of(
                &class.get_property("prototype"),
                &performance_entry_class("PerformanceEntry").get_property("prototype"),
            );
        }
        if matches!(name, "PerformanceMark" | "PerformanceMeasure") {
            class
                .get_property("prototype")
                .set_property("detail", Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn performance_timeline_class(name: &'static str) -> Value {
    PERFORMANCE_TIMELINE_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let Some((parent, members)) = performance_timeline_spec(name) else {
            return Value::Undefined;
        };
        let class = finish_class_with_members(
            Value::function(move |_, _| {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string(&format!("{name} is not constructible"))],
                ))
            }),
            name,
            &[],
            members,
        );
        let prototype = class.get_property("prototype");
        if !parent.is_empty() {
            let parent_prototype = if parent == "PerformanceEntry" {
                performance_entry_class("PerformanceEntry").get_property("prototype")
            } else {
                performance_timeline_class(parent).get_property("prototype")
            };
            w3cos_core::class::set_prototype_of(&prototype, &parent_prototype);
        }
        if members.contains(&"toJSON") {
            prototype.set_property(
                "toJSON",
                Value::function(move |this, _| performance_snapshot_json(&this, name)),
            );
        }
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

fn performance_member_default(member: &str) -> Value {
    match member {
        "cancelable" | "hadRecentInput" => Value::Bool(false),
        "element" | "intersectionRect" | "node" | "notRestoredReasons" | "target" | "window" => {
            Value::Null
        }
        "scripts" | "serverTiming" | "sources" => Value::array(vec![]),
        "contentEncoding"
        | "contentType"
        | "deliveryType"
        | "description"
        | "id"
        | "identifier"
        | "initiatorType"
        | "invoker"
        | "invokerType"
        | "name"
        | "nextHopProtocol"
        | "renderBlockingStatus"
        | "sourceFunctionName"
        | "sourceURL"
        | "type"
        | "url"
        | "value"
        | "windowAttribution"
        | "workerFinalSourceType"
        | "workerMatchedSourceType" => Value::string(""),
        _ => Value::Number(0.0),
    }
}

fn performance_snapshot_members(class_name: &str) -> Vec<&'static str> {
    let mut result = Vec::new();
    let mut current = class_name;
    while let Some((parent, members)) = performance_timeline_spec(current) {
        for member in members {
            if *member != "toJSON" && !result.contains(member) {
                result.push(member);
            }
        }
        if parent.is_empty() || parent == "PerformanceEntry" {
            break;
        }
        current = parent;
    }
    result
}

fn performance_snapshot_json(value: &Value, class_name: &str) -> Value {
    let result = Value::object(HashMap::new());
    if performance_class_for_entry_type(&value.get_property("entryType").to_js_string()).is_some() {
        for member in ["name", "entryType", "startTime", "duration"] {
            result.set_property(member, value.get_property(member));
        }
    }
    for member in performance_snapshot_members(class_name) {
        result.set_property(member, value.get_property(member));
    }
    result
}

fn brand_performance_snapshot(value: Value, class_name: &'static str) -> Value {
    let Some(_) = performance_timeline_spec(class_name) else {
        return value;
    };
    for member in performance_snapshot_members(class_name) {
        if value.get_property(member).is_undefined() {
            value.set_property(member, performance_member_default(member));
        }
    }
    if matches!(
        class_name,
        "PerformanceResourceTiming" | "PerformanceNavigationTiming"
    ) {
        let timings = value.get_property("serverTiming");
        value.set_property(
            "serverTiming",
            Value::array(
                timings
                    .iter()
                    .map(|timing| brand_performance_snapshot(timing, "PerformanceServerTiming"))
                    .collect(),
            ),
        );
    }
    match class_name {
        "LayoutShift" => {
            let sources = value.get_property("sources");
            value.set_property(
                "sources",
                Value::array(
                    sources
                        .iter()
                        .map(|source| brand_performance_snapshot(source, "LayoutShiftAttribution"))
                        .collect(),
                ),
            );
        }
        "PerformanceNavigationTiming" => {
            let confidence = value.get_property("confidence");
            if confidence.is_object() {
                value.set_property(
                    "confidence",
                    brand_performance_snapshot(confidence, "PerformanceTimingConfidence"),
                );
            }
        }
        _ => {}
    }
    w3cos_core::class::set_prototype_of(
        &value,
        &performance_timeline_class(class_name).get_property("prototype"),
    );
    value
}

pub fn performance_long_task_class() -> Value {
    PERFORMANCE_LONG_TASK_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(
                    "PerformanceLongTaskTiming is not constructible",
                )],
            ))
        }));
        let prototype = class.get_property("prototype");
        w3cos_core::class::set_prototype_of(
            &prototype,
            &performance_entry_class("PerformanceEntry").get_property("prototype"),
        );
        prototype.set_property("attribution", Value::Undefined);
        prototype.set_property(
            "toJSON",
            Value::function(|this, _| {
                Value::object(HashMap::from([
                    ("name".into(), this.get_property("name")),
                    ("entryType".into(), this.get_property("entryType")),
                    ("startTime".into(), this.get_property("startTime")),
                    ("duration".into(), this.get_property("duration")),
                    ("attribution".into(), this.get_property("attribution")),
                ]))
            }),
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn task_attribution_class() -> Value {
    TASK_ATTRIBUTION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("TaskAttributionTiming is not constructible")],
            ))
        }));
        let prototype = class.get_property("prototype");
        w3cos_core::class::set_prototype_of(
            &prototype,
            &performance_entry_class("PerformanceEntry").get_property("prototype"),
        );
        for name in [
            "containerType",
            "containerSrc",
            "containerId",
            "containerName",
        ] {
            prototype.set_property(name, Value::Undefined);
        }
        prototype.set_property(
            "toJSON",
            Value::function(|this, _| {
                Value::object(HashMap::from([
                    ("name".into(), this.get_property("name")),
                    ("entryType".into(), this.get_property("entryType")),
                    ("startTime".into(), this.get_property("startTime")),
                    ("duration".into(), this.get_property("duration")),
                    ("containerType".into(), this.get_property("containerType")),
                    ("containerSrc".into(), this.get_property("containerSrc")),
                    ("containerId".into(), this.get_property("containerId")),
                    ("containerName".into(), this.get_property("containerName")),
                ]))
            }),
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn visibility_state_entry_class() -> Value {
    let class = VISIBILITY_STATE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("VisibilityStateEntry is not constructible")],
            ))
        }));
        w3cos_core::class::set_prototype_of(
            &class.get_property("prototype"),
            &performance_entry_class("PerformanceEntry").get_property("prototype"),
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    });
    let document_prototype = crate::dom_constructors::prototype("Document");
    for name in ["hidden", "visibilityState", "onvisibilitychange"] {
        document_prototype.set_property(name, Value::Undefined);
    }
    class
}

fn task_attribution_value(attribution: &TaskAttribution) -> Value {
    let value = Value::object(HashMap::from([
        ("name".into(), Value::string(&attribution.name)),
        ("entryType".into(), Value::string("taskattribution")),
        ("startTime".into(), Value::Number(0.0)),
        ("duration".into(), Value::Number(0.0)),
        (
            "containerType".into(),
            Value::string(&attribution.container_type),
        ),
        (
            "containerSrc".into(),
            Value::string(&attribution.container_src),
        ),
        (
            "containerId".into(),
            Value::string(&attribution.container_id),
        ),
        (
            "containerName".into(),
            Value::string(&attribution.container_name),
        ),
    ]));
    value.set_property(
        "toJSON",
        Value::function(|this, _| {
            Value::object(HashMap::from([
                ("name".into(), this.get_property("name")),
                ("entryType".into(), this.get_property("entryType")),
                ("startTime".into(), this.get_property("startTime")),
                ("duration".into(), this.get_property("duration")),
                ("containerType".into(), this.get_property("containerType")),
                ("containerSrc".into(), this.get_property("containerSrc")),
                ("containerId".into(), this.get_property("containerId")),
                ("containerName".into(), this.get_property("containerName")),
            ]))
        }),
    );
    let prototype = task_attribution_class().get_property("prototype");
    prototype.set_property("toJSON", value.get_property("toJSON"));
    w3cos_core::class::set_prototype_of(&value, &prototype);
    value
}

pub fn performance_entry_list_class() -> Value {
    PERFORMANCE_ENTRY_LIST_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(
                    "PerformanceObserverEntryList is not constructible",
                )],
            ))
        }));
        for method in ["getEntries", "getEntriesByName", "getEntriesByType"] {
            class
                .get_property("prototype")
                .set_property(method, Value::function(|_, _| Value::array(vec![])));
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn performance_entry_value(entry: &PerformanceEntry) -> Value {
    let value = Value::object(HashMap::from([
        ("name".to_string(), Value::string(&entry.name)),
        ("entryType".to_string(), Value::string(&entry.entry_type)),
        ("startTime".to_string(), Value::Number(entry.start_time)),
        ("duration".to_string(), Value::Number(entry.duration)),
        ("detail".to_string(), entry.detail.clone()),
    ]));
    if let Some(class_name) = performance_class_for_entry_type(&entry.entry_type) {
        if let Value::Object(detail) = &entry.detail {
            for key in detail.borrow().keys() {
                value.set_property(&key, entry.detail.get_property(&key));
            }
        }
        return brand_performance_snapshot(value, class_name);
    }
    if entry.entry_type == "longtask" {
        value.set_property("attribution", entry.detail.clone());
    }
    value.set_property(
        "toJSON",
        Value::function(|this, _| {
            let result = Value::object(HashMap::from([
                ("name".to_string(), this.get_property("name")),
                ("entryType".to_string(), this.get_property("entryType")),
                ("startTime".to_string(), this.get_property("startTime")),
                ("duration".to_string(), this.get_property("duration")),
                ("detail".to_string(), this.get_property("detail")),
            ]));
            if this.get_property("entryType").to_js_string() == "longtask" {
                result.set_property("attribution", this.get_property("attribution"));
            }
            result
        }),
    );
    let prototype = if entry.entry_type == "longtask" {
        let prototype = performance_long_task_class().get_property("prototype");
        prototype.set_property("toJSON", value.get_property("toJSON"));
        prototype
    } else if entry.entry_type == "visibility-state" {
        visibility_state_entry_class().get_property("prototype")
    } else {
        let class_name = if entry.entry_type == "mark" {
            "PerformanceMark"
        } else {
            "PerformanceMeasure"
        };
        performance_entry_class(class_name).get_property("prototype")
    };
    w3cos_core::class::set_prototype_of(&value, &prototype);
    value
}

fn performance_entry_list(entries: Vec<Value>) -> Value {
    let all = Rc::new(entries);
    let value = Value::object(HashMap::new());
    let all_entries = Rc::clone(&all);
    value.set_property(
        "getEntries",
        Value::function(move |_, _| Value::array(all_entries.as_ref().clone())),
    );
    let named_entries = Rc::clone(&all);
    value.set_property(
        "getEntriesByName",
        Value::function(move |_, args| {
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            let kind = args.get(1).map(Value::to_js_string);
            Value::array(
                named_entries
                    .iter()
                    .filter(|entry| {
                        entry.get_property("name").to_js_string() == name
                            && kind.as_ref().is_none_or(|kind| {
                                entry.get_property("entryType").to_js_string() == *kind
                            })
                    })
                    .cloned()
                    .collect(),
            )
        }),
    );
    let typed_entries = all;
    value.set_property(
        "getEntriesByType",
        Value::function(move |_, args| {
            let kind = args.first().map(Value::to_js_string).unwrap_or_default();
            Value::array(
                typed_entries
                    .iter()
                    .filter(|entry| entry.get_property("entryType").to_js_string() == kind)
                    .cloned()
                    .collect(),
            )
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &performance_entry_list_class().get_property("prototype"),
    );
    value
}

fn schedule_performance_delivery(state: &Rc<RefCell<PerformanceObserverState>>) {
    {
        let mut state = state.borrow_mut();
        if state.scheduled || !state.active {
            return;
        }
        state.scheduled = true;
    }
    let delivery_state = Rc::clone(state);
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        let (callback, observer, entries) = {
            let mut state = delivery_state.borrow_mut();
            state.scheduled = false;
            if !state.active || state.pending.is_empty() {
                return Value::Undefined;
            }
            (
                state.callback.clone(),
                state.observer.clone(),
                std::mem::take(&mut state.pending),
            )
        };
        callback.call(
            Value::Undefined,
            vec![performance_entry_list(entries), observer],
        );
        Value::Undefined
    }));
}

fn record_performance_entry(entry: PerformanceEntry) -> Value {
    let value = performance_entry_value(&entry);
    PERFORMANCE_ENTRIES.with(|entries| entries.borrow_mut().push(entry.clone()));
    PERFORMANCE_OBSERVERS.with(|observers| {
        for observer in observers.borrow().iter() {
            let matches = {
                let state = observer.borrow();
                state.active && state.entry_types.contains(&entry.entry_type)
            };
            if matches {
                observer.borrow_mut().pending.push(value.clone());
                schedule_performance_delivery(observer);
            }
        }
    });
    value
}

/// Record a scheduler-observed long task and deliver it through the standard
/// Performance Timeline and PerformanceObserver paths.
pub fn record_long_task(
    name: &str,
    start_time: f64,
    duration: f64,
    attributions: Vec<TaskAttribution>,
) -> bool {
    if !start_time.is_finite() || start_time < 0.0 || !duration.is_finite() || duration < 0.0 {
        return false;
    }
    record_performance_entry(PerformanceEntry {
        name: name.to_string(),
        entry_type: "longtask".to_string(),
        start_time,
        duration,
        detail: Value::array(attributions.iter().map(task_attribution_value).collect()),
    });
    true
}

pub fn record_visibility_state(state: &str, start_time: f64) -> bool {
    if !matches!(state, "visible" | "hidden") || !start_time.is_finite() || start_time < 0.0 {
        return false;
    }
    record_performance_entry(PerformanceEntry {
        name: state.to_string(),
        entry_type: "visibility-state".to_string(),
        start_time,
        duration: 0.0,
        detail: Value::Null,
    });
    true
}

/// Inject a host-observed rendering, interaction, navigation, resource, or
/// layout entry into the browser-compatible Performance Timeline.
///
/// `detail` is the WebIDL-shaped snapshot for the selected entry type. Missing
/// fields receive the same neutral values used when the native host cannot
/// measure them.
pub fn record_performance_timeline_entry(
    entry_type: &str,
    name: &str,
    start_time: f64,
    duration: f64,
    detail: Value,
) -> bool {
    if !PERFORMANCE_TIMELINE_ENTRY_TYPES.contains(&entry_type)
        || !start_time.is_finite()
        || start_time < 0.0
        || !duration.is_finite()
        || duration < 0.0
        || (!detail.is_object() && !detail.is_undefined())
    {
        return false;
    }
    record_performance_entry(PerformanceEntry {
        name: name.to_string(),
        entry_type: entry_type.to_string(),
        start_time,
        duration,
        detail: if detail.is_undefined() {
            Value::object(HashMap::new())
        } else {
            detail
        },
    });
    true
}

pub fn performance_mark(args: &[Value], now: f64) -> Value {
    let name = args.first().map(Value::to_js_string).unwrap_or_default();
    let options = args.get(1).cloned().unwrap_or_default();
    let start_time = if options.is_object() && !options.get_property("startTime").is_undefined() {
        let start_time = options.get_property("startTime").to_number();
        if start_time < 0.0 {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".to_string(), Value::string("TypeError")),
                (
                    "message".to_string(),
                    Value::string("PerformanceMark startTime must not be negative"),
                ),
            ])));
        }
        start_time
    } else {
        now
    };
    let detail = if options.is_object() && !options.get_property("detail").is_undefined() {
        w3cos_core::web::structured_clone(vec![options.get_property("detail")])
    } else {
        Value::Null
    };
    record_performance_entry(PerformanceEntry {
        name,
        entry_type: "mark".to_string(),
        start_time,
        duration: 0.0,
        detail,
    })
}

fn resolve_performance_time(value: &Value, default: f64) -> f64 {
    if value.is_undefined() {
        return default;
    }
    if value.is_number() {
        return value.to_number();
    }
    let name = value.to_js_string();
    PERFORMANCE_ENTRIES.with(|entries| {
        entries
            .borrow()
            .iter()
            .rev()
            .find(|entry| entry.entry_type == "mark" && entry.name == name)
            .map(|entry| entry.start_time)
            .unwrap_or_else(|| {
                w3cos_core::throw_value(Value::object(HashMap::from([
                    ("name".to_string(), Value::string("SyntaxError")),
                    (
                        "message".to_string(),
                        Value::string(&format!("The mark '{name}' does not exist")),
                    ),
                ])))
            })
    })
}

pub fn performance_measure(args: &[Value], now: f64) -> Value {
    let name = args.first().map(Value::to_js_string).unwrap_or_default();
    let second = args.get(1).cloned().unwrap_or_default();
    let (start_time, end_time, detail) = if second.is_object() {
        let start = second.get_property("start");
        let end = second.get_property("end");
        let duration = second.get_property("duration");
        let start_time = resolve_performance_time(&start, 0.0);
        let end_time = if !end.is_undefined() {
            resolve_performance_time(&end, now)
        } else if !duration.is_undefined() {
            start_time + duration.to_number()
        } else {
            now
        };
        let detail = second.get_property("detail");
        (
            start_time,
            end_time,
            if detail.is_undefined() {
                Value::Null
            } else {
                w3cos_core::web::structured_clone(vec![detail])
            },
        )
    } else {
        (
            resolve_performance_time(&second, 0.0),
            resolve_performance_time(&args.get(2).cloned().unwrap_or_default(), now),
            Value::Null,
        )
    };
    record_performance_entry(PerformanceEntry {
        name,
        entry_type: "measure".to_string(),
        start_time,
        duration: (end_time - start_time).max(0.0),
        detail,
    })
}

pub fn performance_get_entries(name: Option<&str>, entry_type: Option<&str>) -> Value {
    PERFORMANCE_ENTRIES.with(|entries| {
        let mut entries = entries
            .borrow()
            .iter()
            .filter(|entry| name.is_none_or(|name| entry.name == name))
            .filter(|entry| entry_type.is_none_or(|kind| entry.entry_type == kind))
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.start_time.total_cmp(&right.start_time));
        Value::array(entries.iter().map(performance_entry_value).collect())
    })
}

pub fn performance_clear(entry_type: &str, name: Option<&str>) {
    PERFORMANCE_ENTRIES.with(|entries| {
        entries.borrow_mut().retain(|entry| {
            entry.entry_type != entry_type || name.is_some_and(|name| entry.name != name)
        });
    });
}

pub fn reset_performance_timeline() {
    PERFORMANCE_ENTRIES.with(|entries| entries.borrow_mut().clear());
    PERFORMANCE_OBSERVERS.with(|observers| observers.borrow_mut().clear());
}

pub fn reset_mutation_observers() {
    MUTATION_OBSERVERS.with(|observers| observers.borrow_mut().clear());
}

pub fn reset_intersection_observers() {
    INTERSECTION_OBSERVERS.with(|observers| observers.borrow_mut().clear());
}

pub fn reset_resize_observers() {
    DOM_RESIZE_OBSERVERS.with(|observers| observers.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_dom::node::NodeId;

    #[test]
    fn observer_classes_expose_browser_prototype_members() {
        for (class, methods) in [
            (
                resize_observer_class(),
                &["disconnect", "observe", "unobserve"][..],
            ),
            (
                mutation_observer_class(),
                &["disconnect", "observe", "takeRecords"][..],
            ),
            (
                intersection_observer_class(),
                &["disconnect", "observe", "takeRecords", "unobserve"][..],
            ),
        ] {
            let prototype = class.get_property("prototype");
            for method in methods {
                assert!(
                    prototype.get_property(method).is_function(),
                    "{method} should be exposed on the observer prototype"
                );
            }
        }
        let prototype = intersection_observer_class().get_property("prototype");
        for property in [
            "delay",
            "root",
            "rootMargin",
            "scrollMargin",
            "thresholds",
            "trackVisibility",
        ] {
            assert!(prototype.get_property(property).is_undefined());
        }
    }

    #[test]
    fn intersection_observer_tracks_geometry_thresholds_and_records() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(100.0, 100.0);
        let document = crate::jsdom::document_value();
        let target = document.call_method("createElement", vec![Value::string("div")]);
        document
            .get_property("body")
            .call_method("appendChild", vec![target.clone()]);
        let target_id = crate::jsdom::node_id_of(&target).unwrap();
        crate::dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(90.0, 90.0, 20.0, 20.0),
            );
        });

        let delivered = Rc::new(RefCell::new(Vec::<Value>::new()));
        let delivered_for_callback = Rc::clone(&delivered);
        let observer = w3cos_core::class::construct(
            &intersection_observer_class(),
            vec![
                Value::function(move |_, args| {
                    delivered_for_callback.borrow_mut().extend(args[0].iter());
                    Value::Undefined
                }),
                Value::object(HashMap::from([
                    ("rootMargin".to_string(), Value::string("10px")),
                    (
                        "threshold".to_string(),
                        Value::array(vec![
                            Value::Number(1.0),
                            Value::Number(0.5),
                            Value::Number(0.5),
                            Value::Number(0.0),
                        ]),
                    ),
                ])),
            ],
        );
        assert_eq!(
            observer.get_property("rootMargin").to_js_string(),
            "10px 10px 10px 10px"
        );
        assert_eq!(
            observer.get_property("thresholds").to_js_string(),
            "0,0.5,1"
        );
        observer.call_method("observe", vec![target.clone()]);
        assert!(delivered.borrow().is_empty());
        assert_eq!(crate::jsdom::drain_microtasks(), 1);
        assert_eq!(delivered.borrow().len(), 1);
        assert_eq!(
            delivered.borrow()[0]
                .get_property("intersectionRatio")
                .to_number(),
            1.0
        );
        assert!(w3cos_core::class::instance_of(
            &delivered.borrow()[0].get_property("rootBounds"),
            &crate::geometry_web::class("DOMRectReadOnly")
        ));
        assert!(w3cos_core::class::instance_of(
            &delivered.borrow()[0],
            &intersection_observer_entry_class()
        ));

        crate::dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(95.0, 95.0, 20.0, 20.0),
            );
        });
        refresh_intersection_observers();
        assert_eq!(crate::jsdom::drain_microtasks(), 1);
        assert!(
            (delivered.borrow()[1]
                .get_property("intersectionRatio")
                .to_number()
                - 0.5625)
                .abs()
                < 0.0001
        );

        crate::dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(200.0, 200.0, 20.0, 20.0),
            );
        });
        refresh_intersection_observers();
        assert_eq!(crate::jsdom::drain_microtasks(), 1);
        assert!(
            !delivered.borrow()[2]
                .get_property("isIntersecting")
                .to_bool()
        );

        crate::dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(50.0, 50.0, 20.0, 20.0),
            );
        });
        refresh_intersection_observers();
        assert_eq!(
            observer
                .call_method("takeRecords", vec![])
                .get_property("length")
                .to_u32(),
            1
        );
        assert_eq!(crate::jsdom::drain_microtasks(), 1);
        assert_eq!(delivered.borrow().len(), 3);
        observer.call_method("unobserve", vec![target]);
        refresh_intersection_observers();
        assert_eq!(crate::jsdom::drain_microtasks(), 0);
    }

    #[test]
    fn resize_observer_tracks_dom_box_sizes_and_device_pixels() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_device_pixel_ratio(2.0);
        let document = crate::jsdom::document_value();
        let target = document.call_method("createElement", vec![Value::string("div")]);
        document
            .get_property("body")
            .call_method("appendChild", vec![target.clone()]);
        target
            .get_property("style")
            .set_property("paddingLeft", Value::string("10px"));
        target
            .get_property("style")
            .set_property("paddingRight", Value::string("10px"));
        target
            .get_property("style")
            .set_property("paddingTop", Value::string("5px"));
        target
            .get_property("style")
            .set_property("paddingBottom", Value::string("5px"));
        let target_id = crate::jsdom::node_id_of(&target).unwrap();
        crate::dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(10.0, 20.0, 120.0, 80.0),
            );
        });

        let delivered = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observer_matches = Rc::new(RefCell::new(false));
        let delivered_for_callback = Rc::clone(&delivered);
        let observer_matches_for_callback = Rc::clone(&callback_observer_matches);
        let observer_slot = Rc::new(RefCell::new(Value::Undefined));
        let observer_for_callback = Rc::clone(&observer_slot);
        let observer = w3cos_core::class::construct(
            &resize_observer_class(),
            vec![Value::function(move |_, args| {
                delivered_for_callback.borrow_mut().extend(args[0].iter());
                *observer_matches_for_callback.borrow_mut() =
                    args.get(1) == Some(&*observer_for_callback.borrow());
                Value::Undefined
            })],
        );
        *observer_slot.borrow_mut() = observer.clone();
        observer.call_method(
            "observe",
            vec![
                target.clone(),
                Value::object(HashMap::from([(
                    "box".to_string(),
                    Value::string("device-pixel-content-box"),
                )])),
            ],
        );
        refresh_resize_observers();
        assert_eq!(delivered.borrow().len(), 1);
        assert!(*callback_observer_matches.borrow());
        let entry = delivered.borrow()[0].clone();
        assert_eq!(
            entry
                .get_property("contentRect")
                .get_property("width")
                .to_number(),
            100.0
        );
        assert_eq!(
            entry
                .get_property("borderBoxSize")
                .get_property("0")
                .get_property("inlineSize")
                .to_number(),
            120.0
        );
        assert_eq!(
            entry
                .get_property("contentBoxSize")
                .get_property("0")
                .get_property("blockSize")
                .to_number(),
            70.0
        );
        assert_eq!(
            entry
                .get_property("devicePixelContentBoxSize")
                .get_property("0")
                .get_property("inlineSize")
                .to_number(),
            200.0
        );
        assert!(w3cos_core::class::instance_of(
            &entry.get_property("contentRect"),
            &crate::geometry_web::class("DOMRectReadOnly")
        ));
        assert!(w3cos_core::class::instance_of(
            &entry,
            &resize_observer_entry_class()
        ));
        for property in [
            "borderBoxSize",
            "contentBoxSize",
            "devicePixelContentBoxSize",
        ] {
            assert!(w3cos_core::class::instance_of(
                &entry.get_property(property).get_property("0"),
                &resize_observer_size_class()
            ));
        }

        refresh_resize_observers();
        assert_eq!(delivered.borrow().len(), 1);
        crate::dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(10.0, 20.0, 130.0, 80.0),
            );
        });
        refresh_resize_observers();
        assert_eq!(delivered.borrow().len(), 2);
        observer.call_method("unobserve", vec![target]);
        refresh_resize_observers();
        assert_eq!(delivered.borrow().len(), 2);
    }

    #[test]
    fn performance_entries_and_lists_have_standard_prototype_identity() {
        crate::jsdom::reset_bridge();
        let delivered = Rc::new(RefCell::new(Value::Undefined));
        let delivered_for_callback = Rc::clone(&delivered);
        let observer = w3cos_core::class::construct(
            &performance_observer_class(),
            vec![Value::function(move |_, args| {
                *delivered_for_callback.borrow_mut() = args.first().cloned().unwrap_or_default();
                Value::Undefined
            })],
        );
        observer.call_method(
            "observe",
            vec![Value::object(HashMap::from([(
                "entryTypes".to_string(),
                Value::array(vec![Value::string("mark"), Value::string("measure")]),
            )]))],
        );
        performance_mark(
            &[
                Value::string("start"),
                Value::object(HashMap::from([(
                    "startTime".to_string(),
                    Value::Number(5.0),
                )])),
            ],
            10.0,
        );
        performance_measure(
            &[
                Value::string("span"),
                Value::object(HashMap::from([
                    ("start".to_string(), Value::string("start")),
                    ("end".to_string(), Value::Number(9.0)),
                ])),
            ],
            10.0,
        );
        assert_eq!(crate::jsdom::drain_microtasks(), 1);

        let list = delivered.borrow().clone();
        assert!(w3cos_core::class::instance_of(
            &list,
            &performance_entry_list_class()
        ));
        let entries = list.call_method("getEntries", vec![]);
        let mark = entries.get_property("0");
        let measure = entries.get_property("1");
        assert!(w3cos_core::class::instance_of(
            &mark,
            &performance_entry_class("PerformanceMark")
        ));
        assert!(w3cos_core::class::instance_of(
            &mark,
            &performance_entry_class("PerformanceEntry")
        ));
        assert!(w3cos_core::class::instance_of(
            &measure,
            &performance_entry_class("PerformanceMeasure")
        ));
        assert!(w3cos_core::class::instance_of(
            &measure,
            &performance_entry_class("PerformanceEntry")
        ));
        assert_eq!(measure.get_property("duration").to_number(), 4.0);

        let constructed = w3cos_core::class::construct(
            &performance_entry_class("PerformanceMark"),
            vec![
                Value::string("manual"),
                Value::object(HashMap::from([(
                    "startTime".to_string(),
                    Value::Number(7.0),
                )])),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &constructed,
            &performance_entry_class("PerformanceMark")
        ));
        assert_eq!(constructed.get_property("startTime").to_number(), 7.0);
    }

    #[test]
    fn long_tasks_use_timeline_and_attribution_identities() {
        reset_performance_timeline();
        let delivered = Rc::new(RefCell::new(Value::Undefined));
        let delivered_for_callback = Rc::clone(&delivered);
        let observer = w3cos_core::class::construct(
            &performance_observer_class(),
            vec![Value::function(move |_, args| {
                *delivered_for_callback.borrow_mut() = args[0].call_method("getEntries", vec![]);
                Value::Undefined
            })],
        );
        observer.call_method(
            "observe",
            vec![Value::object(HashMap::from([(
                "type".into(),
                Value::string("longtask"),
            )]))],
        );
        assert!(record_long_task(
            "self",
            12.0,
            75.0,
            vec![TaskAttribution {
                name: "same-origin-window".into(),
                container_type: "window".into(),
                container_id: "app".into(),
                ..TaskAttribution::default()
            }],
        ));
        crate::jsdom::drain_microtasks();
        let task = delivered.borrow().get_property("0");
        assert!(w3cos_core::class::instance_of(
            &task,
            &performance_long_task_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &task,
            &performance_entry_class("PerformanceEntry")
        ));
        let attribution = task.get_property("attribution").get_property("0");
        assert!(w3cos_core::class::instance_of(
            &attribution,
            &task_attribution_class()
        ));
        assert_eq!(
            attribution.get_property("containerId").to_js_string(),
            "app"
        );
    }

    #[test]
    fn host_timeline_entries_preserve_inheritance_and_nested_identities() {
        reset_performance_timeline();
        let delivered = Rc::new(RefCell::new(Value::Undefined));
        let delivered_for_callback = Rc::clone(&delivered);
        let observer = w3cos_core::class::construct(
            &performance_observer_class(),
            vec![Value::function(move |_, args| {
                *delivered_for_callback.borrow_mut() = args[0].call_method("getEntries", vec![]);
                Value::Undefined
            })],
        );
        observer.call_method(
            "observe",
            vec![Value::object(HashMap::from([(
                "entryTypes".into(),
                Value::array(vec![
                    Value::string("navigation"),
                    Value::string("layout-shift"),
                ]),
            )]))],
        );
        assert!(record_performance_timeline_entry(
            "navigation",
            "https://example.test/",
            0.0,
            18.0,
            Value::object(HashMap::from([
                ("responseStatus".into(), Value::Number(200.0)),
                (
                    "serverTiming".into(),
                    Value::array(vec![Value::object(HashMap::from([(
                        "name".into(),
                        Value::string("app"),
                    )]))]),
                ),
                (
                    "confidence".into(),
                    Value::object(HashMap::from([("value".into(), Value::string("high"),)])),
                ),
            ])),
        ));
        assert!(record_performance_timeline_entry(
            "layout-shift",
            "",
            20.0,
            0.0,
            Value::object(HashMap::from([
                ("value".into(), Value::Number(0.1)),
                (
                    "sources".into(),
                    Value::array(vec![Value::object(HashMap::new())]),
                ),
            ])),
        ));
        assert_eq!(crate::jsdom::drain_microtasks(), 1);

        let navigation = delivered.borrow().get_property("0");
        assert!(w3cos_core::class::instance_of(
            &navigation,
            &performance_timeline_class("PerformanceNavigationTiming")
        ));
        assert!(w3cos_core::class::instance_of(
            &navigation,
            &performance_timeline_class("PerformanceResourceTiming")
        ));
        assert!(w3cos_core::class::instance_of(
            &navigation,
            &performance_entry_class("PerformanceEntry")
        ));
        assert_eq!(navigation.get_property("responseStatus").to_number(), 200.0);
        assert!(w3cos_core::class::instance_of(
            &navigation.get_property("serverTiming").get_property("0"),
            &performance_timeline_class("PerformanceServerTiming")
        ));
        assert!(w3cos_core::class::instance_of(
            &navigation.get_property("confidence"),
            &performance_timeline_class("PerformanceTimingConfidence")
        ));
        assert_eq!(
            navigation
                .call_method("toJSON", vec![])
                .get_property("entryType")
                .to_js_string(),
            "navigation"
        );

        let shift = delivered.borrow().get_property("1");
        assert!(w3cos_core::class::instance_of(
            &shift,
            &performance_timeline_class("LayoutShift")
        ));
        assert!(w3cos_core::class::instance_of(
            &shift.get_property("sources").get_property("0"),
            &performance_timeline_class("LayoutShiftAttribution")
        ));
    }

    #[test]
    fn document_visibility_changes_publish_timeline_entries() {
        crate::jsdom::reset_bridge();
        let delivered = Rc::new(RefCell::new(Value::Undefined));
        let delivered_for_callback = Rc::clone(&delivered);
        let observer = w3cos_core::class::construct(
            &performance_observer_class(),
            vec![Value::function(move |_, args| {
                *delivered_for_callback.borrow_mut() =
                    args[0].call_method("getEntries", vec![]).get_property("0");
                Value::Undefined
            })],
        );
        observer.call_method(
            "observe",
            vec![Value::object(HashMap::from([(
                "type".into(),
                Value::string("visibility-state"),
            )]))],
        );
        assert!(crate::jsdom::set_document_visibility("hidden"));
        crate::jsdom::drain_microtasks();
        let entry = delivered.borrow().clone();
        assert!(w3cos_core::class::instance_of(
            &entry,
            &visibility_state_entry_class()
        ));
        assert_eq!(entry.get_property("name").to_js_string(), "hidden");
        assert!(
            crate::jsdom::document_value()
                .get_property("hidden")
                .to_bool()
        );
    }

    #[test]
    fn navigation_restore_diagnostics_preserve_nested_browser_shape() {
        let detail = navigation_diagnostic_value(
            "NotRestoredReasonDetails",
            Value::object(HashMap::from([(
                "reason".into(),
                Value::string("unload-listener"),
            )])),
        );
        let reasons = navigation_diagnostic_value(
            "NotRestoredReasons",
            Value::object(HashMap::from([
                ("id".into(), Value::string("top")),
                ("reasons".into(), Value::array(vec![detail.clone()])),
            ])),
        );
        assert!(w3cos_core::class::instance_of(
            &detail,
            &navigation_diagnostic_class("NotRestoredReasonDetails")
        ));
        assert!(w3cos_core::class::instance_of(
            &reasons,
            &navigation_diagnostic_class("NotRestoredReasons")
        ));
        assert_eq!(
            reasons
                .call_method("toJSON", vec![])
                .get_property("reasons")
                .get_property("0")
                .get_property("reason")
                .to_js_string(),
            "unload-listener"
        );
        assert_eq!(
            reasons
                .get_property("children")
                .get_property("length")
                .to_u32(),
            0
        );
    }
}
