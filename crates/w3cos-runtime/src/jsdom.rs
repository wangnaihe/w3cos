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
//! - [`document_value`] / [`window_value`] — the global `document` / `window`
//!   singletons for compiled JS.
//! - [`drain_microtasks`] — runs queued microtasks AND delivers DOM events
//!   that were dispatched through the native w3cos-dom path (see below).
//! - [`tick_timers`] — fires due `setTimeout`/`setInterval` callbacks and
//!   drained `requestAnimationFrame` callbacks.
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
//! # Frame-loop integration (NOT wired here)
//!
//! The window frame loop should call, once per frame:
//! `jsdom::tick_timers(); jsdom::drain_microtasks();`
//! `window.rs` is intentionally not modified by this module.
//!
//! # Timer model
//!
//! JS timers are kept in a bridge-side store rather than
//! [`crate::timers`]: `timers::set_timeout` only accepts `EventAction`, which
//! has no JS-callback variant; framework adapters use the separate DOM host
//! boundary (`Notify` would fire desktop notifications via `state::execute_action`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use w3cos_core::{JsObject, ProxyBuilder, Value};
use w3cos_dom::Element;
use w3cos_dom::events::{Event, EventData, EventType};
use w3cos_dom::node::NodeId;

use crate::dom;

// ── Bridge state (all thread-local, matching the thread-local Document) ────

struct JsListener {
    node: u32,
    event_type: EventType,
    handler: Value,
    capture: bool,
}

struct JsTimer {
    id: u32,
    callback: Value,
    args: Vec<Value>,
    fire_at: Instant,
    interval: Option<Duration>,
}

thread_local! {
    /// node id → memoized element Value (identity: `a === b` via Rc::ptr_eq).
    static ELEMENT_VALUES: RefCell<HashMap<u32, Value>> = RefCell::new(HashMap::new());
    /// (node, key) → JS expando properties assigned through the set trap
    /// (plus bridge-cached "style"/"classList"/"__ctx2d" values).
    static ELEMENT_PROPS: RefCell<HashMap<(u32, String), Value>> = RefCell::new(HashMap::new());
    /// (node, kebab-prop) → raw CSS value cache. `CSSStyleDeclaration` drops
    /// properties it does not know, so reads of e.g. `lineHeight` would
    /// otherwise come back "".
    static STYLE_CACHE: RefCell<HashMap<(u32, String), String>> = RefCell::new(HashMap::new());
    /// JS event listener registry. Delivery for native events consults this
    /// at drain time; `dispatchEvent` consults it synchronously.
    static LISTENERS: RefCell<Vec<JsListener>> = RefCell::new(Vec::new());
    /// (node, event_type) pairs that already have a native snapshot closure
    /// registered inside w3cos-dom's EventRegistry.
    static NATIVELY_REGISTERED: RefCell<HashSet<(u32, EventType)>> = RefCell::new(HashSet::new());
    /// Event snapshots taken by native snapshot closures, awaiting delivery.
    static PENDING_EVENTS: RefCell<Vec<Event>> = RefCell::new(Vec::new());
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
    /// requestAnimationFrame callbacks: (id, callback).
    static RAF_QUEUE: RefCell<Vec<(u32, Value)>> = RefCell::new(Vec::new());
    static NEXT_RAF_ID: Cell<u32> = Cell::new(1);
    /// Viewport (width, height, devicePixelRatio) for window/matchMedia.
    static VIEWPORT: Cell<(f64, f64, f64)> = Cell::new((1024.0, 768.0, 1.0));
    /// Bridge-side focus tracking (no real input focus exists yet).
    static ACTIVE_ELEMENT: RefCell<Option<u32>> = RefCell::new(None);
    /// Lazily-created <html> / <head> elements.
    static HTML_ID: RefCell<Option<u32>> = RefCell::new(None);
    static HEAD_ID: RefCell<Option<u32>> = RefCell::new(None);
    /// Singletons. Their contents are stateless (all data is read from the
    /// DOM/viewport lazily), so they survive `reset_bridge`.
    static DOCUMENT_VALUE: RefCell<Option<Value>> = RefCell::new(None);
    static WINDOW_VALUE: RefCell<Option<Value>> = RefCell::new(None);
    static SELECTION_VALUE: RefCell<Option<Value>> = RefCell::new(None);
    /// Canvas 2D contexts per canvas node.
    static CANVAS_CONTEXTS: RefCell<HashMap<u32, Rc<RefCell<crate::canvas2d::CanvasRenderingContext2D>>>> =
        RefCell::new(HashMap::new());
    /// In-memory sessionStorage.
    static SESSION_STORAGE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Clipboard fallback for non-desktop targets.
    static CLIPBOARD_FALLBACK: RefCell<String> = RefCell::new(String::new());
    /// performance.now() origin.
    static START_TIME: Instant = Instant::now();
    /// PRNG state for crypto.getRandomValues (xorshift64).
    static RNG_STATE: Cell<u64> = Cell::new(0);
}

// ── Small helpers ──────────────────────────────────────────────────────────

fn func(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    Value::function(f)
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undefined)
}

fn get_expando(node: u32, key: &str) -> Option<Value> {
    ELEMENT_PROPS.with(|p| p.borrow().get(&(node, key.to_string())).cloned())
}

fn set_expando(node: u32, key: &str, value: Value) {
    ELEMENT_PROPS.with(|p| p.borrow_mut().insert((node, key.to_string()), value));
}

/// Extract the DOM node id carried by an element Value (`__node_id` hidden
/// prop, read directly so the proxy trap is bypassed).
pub fn node_id_of(value: &Value) -> Option<u32> {
    if let Value::Object(obj) = value {
        let direct = obj.borrow().get_direct("__node_id");
        if direct.is_number() {
            return Some(direct.to_u32());
        }
    }
    None
}

fn performance_now() -> f64 {
    START_TIME.with(|t| t.elapsed().as_secs_f64() * 1000.0)
}

fn js_array(items: Vec<Value>) -> Value {
    Value::array(items)
}

fn element_or_null(node: Option<u32>) -> Value {
    match node {
        Some(id) => element_value(id),
        None => Value::Null,
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

fn matches_simple(selector: &str, node: u32) -> bool {
    if selector.is_empty() || selector.contains(['>', '+', '~', ':', '[', ']', '*']) {
        return false;
    }
    if dom::node_type(node) != 1 {
        return false;
    }
    // #id part
    if let Some(hash) = selector.find('#') {
        let id: String = selector[hash + 1..]
            .chars()
            .take_while(|c| *c != '.' && *c != '#')
            .collect();
        if dom::get_attribute(node, "id").as_deref() != Some(id.as_str()) {
            return false;
        }
    }
    // .class parts
    for cls in selector.split('.').skip(1) {
        let cls: String = cls.chars().take_while(|c| *c != '#').collect();
        if cls.is_empty() {
            continue;
        }
        if !dom::class_list_contains(node, &cls) {
            return false;
        }
    }
    // tag part (leading run before '.' or '#')
    let tag: String = selector
        .chars()
        .take_while(|c| *c != '.' && *c != '#')
        .collect();
    if !tag.is_empty() && dom::tag_name(node) != tag.to_ascii_lowercase() {
        return false;
    }
    true
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

/// Right-to-left descendant-combinator matching against the ancestor chain.
fn matches_selector_chain(node: u32, parts: &[&str]) -> bool {
    if parts.is_empty() || !matches_simple(parts[parts.len() - 1], node) {
        return false;
    }
    let mut pi = parts.len() - 1;
    let mut cur = dom::parent_node(node);
    while pi > 0 {
        let target = parts[pi - 1];
        let mut found = false;
        let mut ancestor = cur;
        while let Some(id) = ancestor {
            if matches_simple(target, id) {
                cur = dom::parent_node(id);
                found = true;
                break;
            }
            ancestor = dom::parent_node(id);
        }
        if !found {
            return false;
        }
        pi -= 1;
    }
    true
}

/// Candidate nodes for the right-most simple selector, using the document's
/// id/class/tag indexes for speed.
fn selector_candidates(simple: &str) -> Vec<u32> {
    if let Some(hash) = simple.find('#') {
        let id: String = simple[hash + 1..]
            .chars()
            .take_while(|c| *c != '.' && *c != '#')
            .collect();
        return dom::get_element_by_id(&id).into_iter().collect();
    }
    if let Some(dot) = simple.find('.') {
        let first_class: String = simple[dot + 1..]
            .chars()
            .take_while(|c| *c != '.' && *c != '#')
            .collect();
        if !first_class.is_empty() {
            return dom::get_elements_by_class_name(&first_class);
        }
    }
    let tag: String = simple
        .chars()
        .take_while(|c| *c != '.' && *c != '#')
        .collect();
    if tag.is_empty() {
        Vec::new()
    } else {
        dom::get_elements_by_tag_name(&tag.to_ascii_lowercase())
    }
}

fn query_selector_all_scoped(scope: Option<u32>, selector: &str) -> Vec<u32> {
    let selector = selector.trim();
    let parts: Vec<&str> = selector.split_whitespace().collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let candidates = selector_candidates(parts[parts.len() - 1]);
    candidates
        .into_iter()
        .filter(|&c| {
            scope.is_none_or(|s| is_ancestor_of(s, c)) && matches_selector_chain(c, &parts)
        })
        .collect()
}

// ── Element values ─────────────────────────────────────────────────────────

/// Get (or create) the JS `Value` for a DOM node. Memoized per node so
/// identity comparisons (`parent.appendChild(x) === x`) hold.
pub fn element_value(node: u32) -> Value {
    if let Some(v) = ELEMENT_VALUES.with(|c| c.borrow().get(&node).cloned()) {
        return v;
    }
    let value = build_element_value(node);
    ELEMENT_VALUES.with(|c| c.borrow_mut().insert(node, value.clone()));
    value
}

fn build_element_value(node: u32) -> Value {
    let mut props = HashMap::new();
    props.insert("__node_id".to_string(), Value::Number(node as f64));

    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
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
        .set(move |_target, key, value, _receiver| element_computed_set(node, key, value))
        .build();

    Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(props, handler))))
}

fn child_elements(node: u32) -> Vec<u32> {
    dom::children(node)
        .into_iter()
        .filter(|&c| dom::node_type(c) == 1)
        .collect()
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
    }
}

fn decode_html_entities(input: &str) -> String {
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

fn html_tag_end(input: &str) -> Option<usize> {
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

fn parse_html_attributes(mut input: &str) -> Vec<(String, String)> {
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

fn apply_html_attribute(node: u32, name: &str, value: &str) {
    match name {
        "class" => dom::set_class_name(node, value),
        "style" => {
            dom::set_attribute(node, name, value);
            parse_css_text(node, value);
        }
        _ => dom::set_attribute(node, name, value),
    }
}

/// Parse a trusted HTML fragment into real DOM nodes. Monaco's view layer
/// renders visible lines through `innerHTML`, so treating markup as a text
/// node leaves an otherwise healthy editor visually empty.
fn append_html_fragment(parent: u32, html: &str) {
    let mut stack = vec![parent];
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(text_end) = rest.find('<') {
            if text_end > 0 {
                let text = decode_html_entities(&rest[..text_end]);
                if !text.is_empty() {
                    let text_node = dom::create_text_node(&text);
                    dom::append_child(*stack.last().unwrap_or(&parent), text_node);
                }
                rest = &rest[text_end..];
                continue;
            }
        } else {
            let text = decode_html_entities(rest);
            if !text.is_empty() {
                let text_node = dom::create_text_node(&text);
                dom::append_child(*stack.last().unwrap_or(&parent), text_node);
            }
            break;
        }

        if let Some(after_comment) = rest.strip_prefix("<!--") {
            rest = after_comment
                .find("-->")
                .map(|end| &after_comment[end + 3..])
                .unwrap_or("");
            continue;
        }
        let Some(end) = html_tag_end(rest) else {
            let text_node = dom::create_text_node(rest);
            dom::append_child(*stack.last().unwrap_or(&parent), text_node);
            break;
        };
        let mut tag = rest[1..end].trim();
        rest = &rest[end + 1..];
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        if let Some(closing) = tag.strip_prefix('/') {
            let closing = closing.trim().to_ascii_lowercase();
            while stack.len() > 1 {
                let current = stack.pop().unwrap();
                if dom::tag_name(current) == closing {
                    break;
                }
            }
            continue;
        }

        let self_closing = tag.ends_with('/');
        if self_closing {
            tag = tag[..tag.len() - 1].trim_end();
        }
        let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
        let name = tag[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let element = dom::create_element(&name);
        for (attribute, value) in parse_html_attributes(&tag[name_end..]) {
            apply_html_attribute(element, &attribute, &value);
        }
        dom::append_child(*stack.last().unwrap_or(&parent), element);
        let is_void = matches!(
            name.as_str(),
            "area"
                | "base"
                | "br"
                | "col"
                | "embed"
                | "hr"
                | "img"
                | "input"
                | "link"
                | "meta"
                | "param"
                | "source"
                | "track"
                | "wbr"
        );
        if !self_closing && !is_void {
            stack.push(element);
        }
    }
}

fn rect_value(rect: w3cos_dom::DOMRect) -> Value {
    let mut props = HashMap::new();
    let insert = |m: &mut HashMap<String, Value>, k: &str, v: f32| {
        m.insert(k.to_string(), Value::Number(v as f64));
    };
    insert(&mut props, "x", rect.x);
    insert(&mut props, "y", rect.y);
    insert(&mut props, "width", rect.width);
    insert(&mut props, "height", rect.height);
    insert(&mut props, "top", rect.top());
    insert(&mut props, "left", rect.left());
    insert(&mut props, "right", rect.right());
    insert(&mut props, "bottom", rect.bottom());
    Value::object(props)
}

fn is_inputish(node: u32) -> bool {
    matches!(
        dom::tag_name(node).as_str(),
        "input" | "textarea" | "select" | "option"
    )
}

fn element_computed_get(node: u32, key: &str) -> Value {
    match key {
        // ── Node identity ──
        "nodeType" => Value::Number(dom::node_type(node) as f64),
        "nodeName" => Value::string(&dom::node_name(node)),
        "localName" => {
            if dom::node_type(node) == 1 {
                Value::string(&dom::tag_name(node))
            } else {
                Value::Undefined
            }
        }
        "tagName" => {
            if dom::node_type(node) == 1 {
                Value::string(&dom::tag_name(node).to_ascii_uppercase())
            } else {
                Value::Undefined
            }
        }
        "namespaceURI" => Value::string("http://www.w3.org/1999/xhtml"),
        "ownerDocument" => document_value(),
        "isConnected" => Value::Bool(dom::is_connected(node)),

        // ── Tree traversal ──
        "parentNode" => element_or_null(dom::parent_node(node)),
        "parentElement" => match dom::parent_node(node) {
            Some(p) if dom::node_type(p) == 1 => element_value(p),
            _ => Value::Null,
        },
        "children" => js_array(
            child_elements(node)
                .into_iter()
                .map(element_value)
                .collect(),
        ),
        "childNodes" => js_array(dom::children(node).into_iter().map(element_value).collect()),
        "childElementCount" => Value::Number(child_elements(node).len() as f64),
        "firstChild" => element_or_null(dom::first_child(node)),
        "lastChild" => element_or_null(dom::last_child(node)),
        "nextSibling" => element_or_null(dom::next_sibling(node)),
        "previousSibling" => element_or_null(dom::previous_sibling(node)),
        "firstElementChild" => element_or_null(first_element_child(node)),
        "lastElementChild" => element_or_null(child_elements(node).into_iter().last()),
        "nextElementSibling" => element_or_null(sibling_element(node, true)),
        "previousElementSibling" => element_or_null(sibling_element(node, false)),
        "hasChildNodes" => func(move |_, _| Value::Bool(dom::first_child(node).is_some())),
        "getRootNode" => func(|_, _| document_value()),
        "contains" => func(move |_, args| {
            let Some(other) = node_id_of(&arg(&args, 0)) else {
                return Value::Bool(false);
            };
            Value::Bool(other == node || is_ancestor_of(node, other))
        }),
        "isSameNode" | "isEqualNode" => {
            func(move |_, args| Value::Bool(node_id_of(&arg(&args, 0)) == Some(node)))
        }

        // ── Text content ──
        "textContent" => Value::string(&dom::inner_text(node)),
        "nodeValue" | "data" => match dom::get_text_content(node) {
            Some(t) => Value::string(&t),
            None => Value::Null,
        },
        "innerText" => Value::string(&dom::inner_text(node)),
        "innerHTML" => {
            let mut s = dom::get_text_content(node).unwrap_or_default();
            for c in dom::children(node) {
                s.push_str(&dom::outer_html(c));
            }
            Value::string(&s)
        }
        "outerHTML" => Value::string(&dom::outer_html(node)),

        // ── Attributes ──
        "id" => Value::string(&dom::get_attribute(node, "id").unwrap_or_default()),
        "className" => Value::string(&dom::class_name(node)),
        "classList" => class_list_value(node),
        "attributes" => attributes_value(node),
        "dataset" => {
            let map = dom::with_document(|doc| Element::new(NodeId::from_u32(node)).dataset(doc));
            Value::object(
                map.into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect(),
            )
        }
        "getAttribute" => {
            func(
                move |_, args| match dom::get_attribute(node, &arg(&args, 0).to_js_string()) {
                    Some(v) => Value::String(v),
                    None => Value::Null,
                },
            )
        }
        "getAttributeNS" => func(move |_, args| {
            // Namespace ignored (bridge limitation); same as getAttribute.
            match dom::get_attribute(node, &arg(&args, 1).to_js_string()) {
                Some(v) => Value::String(v),
                None => Value::Null,
            }
        }),
        "setAttribute" => func(move |_, args| {
            dom::set_attribute(
                node,
                &arg(&args, 0).to_js_string(),
                &arg(&args, 1).to_js_string(),
            );
            Value::Undefined
        }),
        "setAttributeNS" => func(move |_, args| {
            dom::set_attribute(
                node,
                &arg(&args, 1).to_js_string(),
                &arg(&args, 2).to_js_string(),
            );
            Value::Undefined
        }),
        "hasAttribute" => func(move |_, args| {
            Value::Bool(dom::has_attribute(node, &arg(&args, 0).to_js_string()))
        }),
        "removeAttribute" => func(move |_, args| {
            dom::remove_attribute(node, &arg(&args, 0).to_js_string());
            Value::Undefined
        }),
        "toggleAttribute" => func(move |_, args| {
            let name = arg(&args, 0).to_js_string();
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

        // ── Tree mutation ──
        "appendChild" => func(move |_, args| {
            let child = arg(&args, 0);
            if let Some(cid) = node_id_of(&child) {
                dom::append_child(node, cid);
            }
            child
        }),
        "removeChild" => func(move |_, args| {
            let child = arg(&args, 0);
            if let Some(cid) = node_id_of(&child) {
                dom::remove_child(node, cid);
            }
            child
        }),
        "insertBefore" => func(move |_, args| {
            let new_child = arg(&args, 0);
            let ref_child = arg(&args, 1);
            if let Some(nid) = node_id_of(&new_child) {
                match node_id_of(&ref_child) {
                    Some(rid) => dom::insert_before(node, nid, rid),
                    None => dom::append_child(node, nid),
                }
            }
            new_child
        }),
        "replaceChild" => func(move |_, args| {
            let new_child = arg(&args, 0);
            let old_child = arg(&args, 1);
            if let (Some(nid), Some(oid)) = (node_id_of(&new_child), node_id_of(&old_child)) {
                dom::replace_child(node, nid, oid);
            }
            old_child
        }),
        "cloneNode" => func(move |_, args| {
            let deep = arg(&args, 0).to_bool();
            element_value(dom::clone_node(node, deep))
        }),
        "remove" => func(move |_, _| {
            if let Some(parent) = dom::parent_node(node) {
                dom::remove_child(parent, node);
            }
            Value::Undefined
        }),
        "append" | "prepend" => {
            let prepend = key == "prepend";
            func(move |_, args| {
                for a in args {
                    let cid = match (&a, node_id_of(&a)) {
                        (Value::String(s), _) => dom::create_text_node(s),
                        (_, Some(id)) => id,
                        _ => continue,
                    };
                    if prepend {
                        match dom::first_child(node) {
                            Some(first) => dom::insert_before(node, cid, first),
                            None => dom::append_child(node, cid),
                        }
                    } else {
                        dom::append_child(node, cid);
                    }
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
                for a in args {
                    let cid = match (&a, node_id_of(&a)) {
                        (Value::String(s), _) => dom::create_text_node(s),
                        (_, Some(id)) => id,
                        _ => continue,
                    };
                    if before {
                        dom::insert_before(parent, cid, node);
                    } else {
                        match dom::next_sibling(node) {
                            Some(next) => dom::insert_before(parent, cid, next),
                            None => dom::append_child(parent, cid),
                        }
                    }
                }
                Value::Undefined
            })
        }
        "replaceWith" => func(move |_, args| {
            if let Some(parent) = dom::parent_node(node) {
                let mut inserted = false;
                for a in &args {
                    let cid = match (a, node_id_of(a)) {
                        (Value::String(s), _) => dom::create_text_node(s),
                        (_, Some(id)) => id,
                        _ => continue,
                    };
                    if !inserted {
                        dom::replace_child(parent, cid, node);
                        inserted = true;
                    } else {
                        dom::append_child(parent, cid);
                    }
                }
                if !inserted {
                    dom::remove_child(parent, node);
                }
            }
            Value::Undefined
        }),
        "replaceChildren" => func(move |_, args| {
            clear_children(node);
            dom::set_text_content(node, "");
            for a in args {
                let cid = match (&a, node_id_of(&a)) {
                    (Value::String(s), _) => dom::create_text_node(s),
                    (_, Some(id)) => id,
                    _ => continue,
                };
                dom::append_child(node, cid);
            }
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

        // ── Selectors ──
        "matches" => func(move |_, args| {
            let sel = arg(&args, 0).to_js_string();
            let parts: Vec<&str> = sel.split_whitespace().collect();
            Value::Bool(matches_selector_chain(node, &parts))
        }),
        "closest" => func(move |_, args| {
            let sel = arg(&args, 0).to_js_string();
            let parts: Vec<&str> = sel.split_whitespace().collect();
            let mut cur = Some(node);
            while let Some(id) = cur {
                if dom::node_type(id) == 1 && matches_selector_chain(id, &parts) {
                    return element_value(id);
                }
                cur = dom::parent_node(id);
            }
            Value::Null
        }),
        "querySelector" => func(move |_, args| {
            let sel = arg(&args, 0).to_js_string();
            element_or_null(
                query_selector_all_scoped(Some(node), &sel)
                    .into_iter()
                    .next(),
            )
        }),
        "querySelectorAll" => func(move |_, args| {
            let sel = arg(&args, 0).to_js_string();
            js_array(
                query_selector_all_scoped(Some(node), &sel)
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
        "getElementsByTagName" => func(move |_, args| {
            let tag = arg(&args, 0).to_js_string().to_ascii_lowercase();
            js_array(
                dom::get_elements_by_tag_name(&tag)
                    .into_iter()
                    .filter(|&c| is_ancestor_of(node, c))
                    .map(element_value)
                    .collect(),
            )
        }),
        "getElementsByClassName" => func(move |_, args| {
            let class = arg(&args, 0).to_js_string();
            js_array(
                dom::get_elements_by_class_name(&class)
                    .into_iter()
                    .filter(|&c| is_ancestor_of(node, c))
                    .map(element_value)
                    .collect(),
            )
        }),

        // ── Layout (zeros until the layout engine runs) ──
        "getBoundingClientRect" => func(move |_, _| rect_value(dom::bounding_rect(node))),
        "getClientRects" => func(move |_, _| js_array(vec![rect_value(dom::bounding_rect(node))])),
        "offsetWidth" | "clientWidth" | "scrollWidth" => {
            Value::Number(dom::bounding_rect(node).width as f64)
        }
        "offsetHeight" | "clientHeight" | "scrollHeight" => {
            Value::Number(dom::bounding_rect(node).height as f64)
        }
        "offsetTop" => Value::Number(dom::bounding_rect(node).y as f64),
        "offsetLeft" => Value::Number(dom::bounding_rect(node).x as f64),
        "offsetParent" => Value::Null,
        "clientTop" | "clientLeft" => Value::Number(0.0),
        "scrollTop" => Value::Number(dom::get_scroll_offset(node).1 as f64),
        "scrollLeft" => Value::Number(dom::get_scroll_offset(node).0 as f64),
        "scrollIntoView" => func(|_, _| Value::Undefined),
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
            ACTIVE_ELEMENT.with(|a| *a.borrow_mut() = Some(node));
            dispatch_sync(node, EventType::Focus, EventData::Focus);
            Value::Undefined
        }),
        "blur" => func(move |_, _| {
            ACTIVE_ELEMENT.with(|a| {
                if *a.borrow() == Some(node) {
                    *a.borrow_mut() = None;
                }
            });
            dispatch_sync(node, EventType::Blur, EventData::Focus);
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
        "value" => {
            if is_inputish(node) {
                Value::string(&dom::get_attribute(node, "value").unwrap_or_default())
            } else {
                Value::Undefined
            }
        }
        "checked" => {
            if is_inputish(node) {
                Value::Bool(dom::has_attribute(node, "checked"))
            } else {
                Value::Undefined
            }
        }
        "disabled" => Value::Bool(dom::has_attribute(node, "disabled")),
        "readOnly" => Value::Bool(dom::has_attribute(node, "readonly")),
        "placeholder" => {
            Value::string(&dom::get_attribute(node, "placeholder").unwrap_or_default())
        }
        "selectionStart" | "selectionEnd" => get_expando(node, key).unwrap_or(Value::Number(0.0)),
        "selectionDirection" => Value::string("none"),
        "setSelectionRange" => func(move |_, args| {
            set_expando(node, "selectionStart", arg(&args, 0));
            set_expando(node, "selectionEnd", arg(&args, 1));
            Value::Undefined
        }),
        "setRangeText" => func(|_, _| Value::Undefined),
        "select" => func(|_, _| Value::Undefined),

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
            if kind == "2d" {
                canvas_context_value(node)
            } else {
                Value::Null
            }
        }),

        // ── Pointer capture (no-op; no real pointer routing yet) ──
        "setPointerCapture" | "releasePointerCapture" => func(|_, _| Value::Undefined),
        "hasPointerCapture" => func(|_, _| Value::Bool(false)),

        // ── Shadow DOM (not supported) ──
        "shadowRoot" => Value::Null,
        "attachShadow" => func(|_, _| Value::Null),

        _ => Value::Undefined,
    }
}

fn element_computed_set(node: u32, key: &str, value: Value) -> bool {
    match key {
        "textContent" => {
            clear_children(node);
            dom::set_text_content(node, &value.to_js_string());
        }
        "nodeValue" | "data" => {
            dom::set_text_content(node, &value.to_js_string());
        }
        "innerText" => {
            clear_children(node);
            dom::set_text_content(node, &value.to_js_string());
        }
        "innerHTML" => {
            clear_children(node);
            dom::set_text_content(node, "");
            append_html_fragment(node, &value.to_js_string());
        }
        "id" => dom::set_attribute(node, "id", &value.to_js_string()),
        "className" => dom::set_class_name(node, &value.to_js_string()),
        "value" => dom::set_attribute(node, "value", &value.to_js_string()),
        "checked" => {
            if value.to_bool() {
                dom::set_attribute(node, "checked", "");
            } else {
                dom::remove_attribute(node, "checked");
            }
        }
        "title" | "dir" | "lang" | "slot" | "placeholder" => {
            dom::set_attribute(node, key, &value.to_js_string());
        }
        "contentEditable" => dom::set_attribute(node, "contenteditable", &value.to_js_string()),
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

fn attributes_value(node: u32) -> Value {
    let attrs: Vec<(String, String)> = dom::with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .attributes
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.clone()))
            .collect()
    });
    let mut props = HashMap::new();
    let len = attrs.len();
    for (i, (name, value)) in attrs.into_iter().enumerate() {
        let mut attr_obj = HashMap::new();
        attr_obj.insert("name".to_string(), Value::string(&name));
        attr_obj.insert("value".to_string(), Value::string(&value));
        attr_obj.insert("specified".to_string(), Value::Bool(true));
        props.insert(i.to_string(), Value::object(attr_obj));
        props.insert(name, Value::string(&value));
    }
    props.insert("length".to_string(), Value::Number(len as f64));
    props.insert(
        "item".to_string(),
        func(move |_, args| {
            let _ = args;
            Value::Null
        }),
    );
    Value::object(props)
}

// ── classList ──────────────────────────────────────────────────────────────

fn class_list_value(node: u32) -> Value {
    if let Some(v) = get_expando(node, "classList") {
        return v;
    }
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "add".to_string(),
        func(move |_, args| {
            for a in args {
                dom::class_list_add(node, &a.to_js_string());
            }
            Value::Undefined
        }),
    );
    props.insert(
        "remove".to_string(),
        func(move |_, args| {
            for a in args {
                dom::class_list_remove(node, &a.to_js_string());
            }
            Value::Undefined
        }),
    );
    props.insert(
        "toggle".to_string(),
        func(move |_, args| {
            let class = arg(&args, 0).to_js_string();
            let force = arg(&args, 1);
            if force.is_undefined() {
                Value::Bool(dom::class_list_toggle(node, &class))
            } else if force.to_bool() {
                dom::class_list_add(node, &class);
                Value::Bool(true)
            } else {
                dom::class_list_remove(node, &class);
                Value::Bool(false)
            }
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
            if dom::class_list_contains(node, &old) {
                dom::class_list_remove(node, &old);
                dom::class_list_add(node, &new);
                Value::Bool(true)
            } else {
                Value::Bool(false)
            }
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
        func(move |_, _| Value::string(&dom::class_name(node))),
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
        func(move |_, _| Value::string(&dom::class_name(node))),
    );
    let value = Value::object(props);
    set_expando(node, "classList", value.clone());
    value
}

// ── style proxy ────────────────────────────────────────────────────────────

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

fn style_apply(node: u32, kebab: &str, value: &str) {
    STYLE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if value.is_empty() {
            cache.remove(&(node, kebab.to_string()));
        } else {
            cache.insert((node, kebab.to_string()), value.to_string());
        }
    });
    // Forward to the typed style (known properties drive layout; unknown ones
    // are dropped there but stay in the bridge cache).
    dom::set_style_property(node, kebab, value);
}

fn style_css_text(node: u32) -> String {
    STYLE_CACHE.with(|c| {
        let cache = c.borrow();
        let mut pairs: Vec<(&String, &String)> = cache
            .iter()
            .filter(|((n, _), _)| *n == node)
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
    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
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
                "length" => Value::Number(
                    STYLE_CACHE.with(|c| c.borrow().keys().filter(|(n, _)| *n == node).count())
                        as f64,
                ),
                // Magic w3cos-core convention keys must stay Undefined:
                // returning "" (a non-undefined value) for `__w3cos_setter_*`
                // makes `Value::set_property` "call" the empty string and
                // skip the proxy set trap entirely.
                _ if key.starts_with("__w3cos_") => Value::Undefined,
                _ => Value::string(&style_read(node, &camel_to_kebab(key))),
            }
        })
        .set(move |_target, key, value, _receiver| {
            if key == "cssText" {
                parse_css_text(node, &value.to_js_string());
            } else {
                style_apply(node, &camel_to_kebab(key), &value.to_js_string());
            }
            true
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    set_expando(node, "style", value.clone());
    value
}

// ── JS event bridge ────────────────────────────────────────────────────────

fn js_add_event_listener(node: u32, type_name: &str, handler: Value, options: Value) {
    if !handler.is_function() {
        return;
    }
    let capture = match &options {
        Value::Bool(b) => *b,
        Value::Object(_) => options.get_property("capture").to_bool(),
        _ => false,
    };
    let et = event_type_for(type_name);
    LISTENERS.with(|l| {
        l.borrow_mut().push(JsListener {
            node,
            event_type: et,
            handler,
            capture,
        })
    });
    ensure_native_registration(node, et);
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
            v.set_property("__pd", Value::Bool(true));
            v.set_property("returnValue", Value::Bool(false));
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
    value
}

/// Synchronous JS dispatch with capture/target/bubble phases. No document
/// borrow is held while JS handlers run, so handlers may mutate the DOM.
/// Returns false when the event was canceled (preventDefault).
fn dispatch_event_to_js(ev: Event) -> bool {
    let target = ev.target.as_u32();
    let trace = std::env::var_os("W3COS_INPUT_TRACE").is_some();
    let mut listener_calls = 0usize;
    // [target, parent, ..., root]
    let mut chain = vec![target];
    let mut cur = dom::parent_node(target);
    while let Some(id) = cur {
        chain.push(id);
        cur = dom::parent_node(id);
    }

    let js_ev = build_event_value(&ev);
    let stopped = |v: &Value| v.get_property("__sp").to_bool();
    let immediate = |v: &Value| v.get_property("__sip").to_bool();

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
        js_ev.set_property("currentTarget", element_value(id));
        js_ev.set_property("eventPhase", Value::Number(1.0));
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
        js_ev.set_property("currentTarget", element_value(target));
        js_ev.set_property("eventPhase", Value::Number(2.0));
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
            js_ev.set_property("currentTarget", element_value(id));
            js_ev.set_property("eventPhase", Value::Number(3.0));
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

fn dispatch_sync(target: u32, et: EventType, data: EventData) -> bool {
    let mut ev = Event::new(et, NodeId::from_u32(target));
    ev.data = data;
    dispatch_event_to_js(ev)
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
    let pointer_allowed = dispatch_sync(
        target,
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
        dispatch_sync(target, mouse_type, EventData::Mouse(mouse))
    } else {
        true
    };
    !pointer_allowed || !mouse_allowed
}

pub(crate) fn dispatch_native_click(target: u32) -> bool {
    !dispatch_sync(target, EventType::Click, EventData::None)
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

/// Compute the value a browser text control would have after applying an
/// edit at its current UTF-16 selection. The actual mutation still happens
/// after the cancelable `beforeinput` phase.
pub(crate) fn text_control_value_after_edit(target: u32, data: &str, input_type: &str) -> String {
    let value = dom::get_attribute(target, "value").unwrap_or_default();
    let len = value.encode_utf16().count();
    let mut start = get_expando(target, "selectionStart")
        .map(|value| value.to_number().max(0.0) as usize)
        .unwrap_or(0)
        .min(len);
    let end = get_expando(target, "selectionEnd")
        .map(|value| value.to_number().max(0.0) as usize)
        .unwrap_or(start)
        .min(len)
        .max(start);
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

    let utf16_to_byte = |offset: usize| {
        if offset == len {
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
    };
    let start_byte = utf16_to_byte(start);
    let end_byte = utf16_to_byte(end);
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
fn js_dispatch_event(node: u32, event_val: Value) -> bool {
    let type_name = event_val.get_property("type").to_js_string();
    if type_name.is_empty() || type_name == "undefined" {
        return true;
    }
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
    dispatch_event_to_js(ev)
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
    let mut props = HashMap::new();
    props.insert("width".to_string(), Value::Number(data.width as f64));
    props.insert("height".to_string(), Value::Number(data.height as f64));
    props.insert(
        "data".to_string(),
        js_array(data.data.iter().map(|b| Value::Number(*b as f64)).collect()),
    );
    Value::object(props)
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

    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            let stored = target.get_property(key);
            if !stored.is_undefined() {
                return stored;
            }
            if let Some(v) = get_expando(node, &format!("ctx:{key}")) {
                return v;
            }
            canvas_ctx_get(node, &ctx_get, key)
        })
        .set(move |_target, key, value, _receiver| {
            set_expando(node, &format!("ctx:{key}"), value.clone());
            match key {
                "fillStyle" => ctx.borrow_mut().set_fill_style(&value.to_js_string()),
                "strokeStyle" => ctx.borrow_mut().set_stroke_style(&value.to_js_string()),
                "lineWidth" => ctx.borrow_mut().set_line_width(value.to_number() as f32),
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
                Value::object(props)
            })
        }
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
            func(move |_, _| {
                ctx.borrow_mut().fill();
                Value::Undefined
            })
        }
        "stroke" => {
            let ctx = ctx.clone();
            func(move |_, _| {
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
        "setLineDash" => func(|_, _| Value::Undefined), // runtime ctx lacks it (gap)
        "getLineDash" => func(|_, _| js_array(vec![])),
        "drawImage" => func(|_, _| Value::Undefined), // v1: no image sources (gap)
        _ => Value::Undefined,
    }
}

// ── Range / Selection ──────────────────────────────────────────────────────

fn range_hidden(v: &Value, key: &str) -> u32 {
    v.get_property(key).to_u32()
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

fn range_value(sc: u32, so: u32, ec: u32, eo: u32) -> Value {
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
                    for (k, val) in [("__sc", n), ("__ec", n), ("__so", 0), ("__eo", 0)] {
                        v.set_property(k, Value::Number(val as f64));
                    }
                }
                Value::Undefined
            }
        }),
    );
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
        "getBoundingClientRect",
        func(|_, _| rect_value(w3cos_dom::DOMRect::zero())),
    );
    value.set_property(
        "getClientRects",
        func(|_, _| js_array(vec![rect_value(w3cos_dom::DOMRect::zero())])),
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
            move |_, _| {
                // Approximation: returns the text contents, not a fragment.
                let r = range_to_w3cos(&v);
                Value::string(&dom::with_document(|doc| r.to_string(doc)))
            }
        }),
    );
    value.set_property(
        "deleteContents",
        func({
            let v = value.clone();
            move |_, _| {
                let r = range_to_w3cos(&v);
                dom::with_document_mut(|doc| r.delete_contents(doc));
                dom::touch_document();
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "extractContents",
        func({
            let v = value.clone();
            move |_, _| {
                // Approximation: returns the extracted text, not a fragment.
                let r = range_to_w3cos(&v);
                let text = dom::with_document_mut(|doc| r.extract_contents(doc));
                dom::touch_document();
                Value::string(&text)
            }
        }),
    );
    value.set_property("detach", func(|_, _| Value::Undefined));
    value.set_property("insertNode", func(|_, _| Value::Undefined)); // gap
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
    SELECTION_VALUE.with(|s| *s.borrow_mut() = Some(value.clone()));
    value
}

// ── document ───────────────────────────────────────────────────────────────

fn head_id() -> u32 {
    ensure_html_structure();
    HEAD_ID.with(|h| h.borrow().unwrap())
}

fn document_element_id() -> u32 {
    ensure_html_structure();
    HTML_ID.with(|h| h.borrow().unwrap())
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

fn fonts_stub() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert("size".to_string(), Value::Number(0.0));
    for name in [
        "add", "delete", "clear", "forEach", "values", "keys", "entries",
    ] {
        props.insert(name.to_string(), func(|_, _| Value::Undefined));
    }
    props.insert("has".to_string(), func(|_, _| Value::Bool(false)));
    props.insert("check".to_string(), func(|_, _| Value::Bool(false)));
    props.insert(
        "load".to_string(),
        func(|_, _| resolved_thenable(js_array(vec![]))),
    );
    props.insert("status".to_string(), Value::string("loaded"));
    let fonts = Value::object(props);
    let f = fonts.clone();
    fonts.set_property("ready", resolved_thenable(f));
    fonts
}

/// The global `document` value (memoized thread-local singleton).
pub fn document_value() -> Value {
    if let Some(v) = DOCUMENT_VALUE.with(|d| d.borrow().clone()) {
        return v;
    }
    let value = build_document_value();
    DOCUMENT_VALUE.with(|d| *d.borrow_mut() = Some(value.clone()));
    value
}

fn build_document_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();

    props.insert("nodeType".to_string(), Value::Number(9.0));
    props.insert("nodeName".to_string(), Value::string("#document"));
    props.insert("hidden".to_string(), Value::Bool(false));
    props.insert("characterSet".to_string(), Value::string("UTF-8"));
    props.insert("compatMode".to_string(), Value::string("CSS1Compat"));
    props.insert("designMode".to_string(), Value::string("off"));
    props.insert("contentType".to_string(), Value::string("text/html"));
    props.insert("documentURI".to_string(), Value::string("w3cos://app"));
    props.insert("URL".to_string(), Value::string("w3cos://app"));
    props.insert("domain".to_string(), Value::string("app"));
    props.insert("referrer".to_string(), Value::string(""));
    props.insert("fonts".to_string(), fonts_stub());

    props.insert(
        "createElement".to_string(),
        func(|_, args| {
            let tag = arg(&args, 0).to_js_string().to_ascii_lowercase();
            element_value(dom::create_element(&tag))
        }),
    );
    props.insert(
        "createElementNS".to_string(),
        func(|_, args| {
            // Namespace recorded on the element value only; the DOM itself is
            // namespace-unaware (documented gap).
            let ns = arg(&args, 0).to_js_string();
            let tag = arg(&args, 1).to_js_string().to_ascii_lowercase();
            let id = dom::create_element(&tag);
            set_expando(id, "namespaceURI", Value::string(&ns));
            element_value(id)
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
        "createDocumentFragment".to_string(),
        func(|_, _| element_value(dom::create_document_fragment())),
    );
    props.insert(
        "getElementById".to_string(),
        func(|_, args| element_or_null(dom::get_element_by_id(&arg(&args, 0).to_js_string()))),
    );
    props.insert(
        "querySelector".to_string(),
        func(|_, args| {
            let sel = arg(&args, 0).to_js_string();
            element_or_null(query_selector_all_scoped(None, &sel).into_iter().next())
        }),
    );
    props.insert(
        "querySelectorAll".to_string(),
        func(|_, args| {
            let sel = arg(&args, 0).to_js_string();
            js_array(
                query_selector_all_scoped(None, &sel)
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
    );
    props.insert(
        "getElementsByTagName".to_string(),
        func(|_, args| {
            let tag = arg(&args, 0).to_js_string().to_ascii_lowercase();
            js_array(
                dom::get_elements_by_tag_name(&tag)
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
    );
    props.insert(
        "getElementsByClassName".to_string(),
        func(|_, args| {
            let class = arg(&args, 0).to_js_string();
            js_array(
                dom::get_elements_by_class_name(&class)
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
    );
    props.insert(
        "createRange".to_string(),
        func(|_, _| range_value(0, 0, 0, 0)),
    );
    props.insert("getSelection".to_string(), func(|_, _| selection_value()));
    props.insert(
        "execCommand".to_string(),
        func(|_, _| Value::Bool(false)), // no clipboard command engine (gap)
    );
    props.insert("hasFocus".to_string(), func(|_, _| Value::Bool(true)));
    props.insert(
        "adoptNode".to_string(),
        func(|_, args| arg(&args, 0)), // single document: no-op
    );
    props.insert(
        "importNode".to_string(),
        func(|_, args| arg(&args, 0)), // single document: no-op
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
        func(|_, _| element_value(dom::body_id())),
    );
    props.insert(
        "__w3cos_getter_head".to_string(),
        func(|_, _| element_value(head_id())),
    );
    props.insert(
        "__w3cos_getter_documentElement".to_string(),
        func(|_, _| element_value(document_element_id())),
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
        func(|_, _| Value::Null),
    );
    props.insert(
        "__w3cos_getter_visibilityState".to_string(),
        func(|_, _| Value::string("visible")),
    );
    props.insert(
        "__w3cos_getter_readyState".to_string(),
        func(|_, _| Value::string("complete")),
    );
    props.insert(
        "__w3cos_getter_cookie".to_string(),
        func(|_, _| Value::string("")), // no cookie jar (gap); assignment works
    );
    props.insert(
        "__w3cos_getter_title".to_string(),
        func(|_, _| Value::string("")),
    );
    props.insert(
        "__w3cos_getter_location".to_string(),
        func(|_, _| location_value()),
    );

    Value::object(props)
}

// ── window ─────────────────────────────────────────────────────────────────

fn resolved_thenable(result: Value) -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "then".to_string(),
        func({
            let r = result.clone();
            move |_, args| {
                let cb = arg(&args, 0);
                if cb.is_function() {
                    let r2 = r.clone();
                    queue_microtask_value(func(move |_, _| {
                        cb.call(Value::Undefined, vec![r2.clone()])
                    }));
                }
                Value::Undefined
            }
        }),
    );
    props.insert("catch".to_string(), func(|_, _| Value::Undefined));
    props.insert(
        "finally".to_string(),
        func(move |_, args| {
            let cb = arg(&args, 0);
            if cb.is_function() {
                queue_microtask_value(func(move |_, _| cb.call(Value::Undefined, vec![])));
            }
            Value::Undefined
        }),
    );
    Value::object(props)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn clipboard_read_text() -> String {
    crate::clipboard::Clipboard::read_text().unwrap_or_default()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn clipboard_read_text() -> String {
    CLIPBOARD_FALLBACK.with(|c| c.borrow().clone())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn clipboard_write_text(text: &str) {
    let _ = crate::clipboard::Clipboard::write_text(text);
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn clipboard_write_text(text: &str) {
    CLIPBOARD_FALLBACK.with(|c| *c.borrow_mut() = text.to_string());
}

fn navigator_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "userAgent".to_string(),
        Value::string("W3COS/0.1 (w3cos; like Gecko)"),
    );
    props.insert("appVersion".to_string(), Value::string("0.1"));
    props.insert("platform".to_string(), Value::string("w3cos"));
    props.insert("vendor".to_string(), Value::string("w3cos"));
    props.insert("language".to_string(), Value::string("en-US"));
    props.insert(
        "languages".to_string(),
        js_array(vec![Value::string("en-US")]),
    );
    props.insert("maxTouchPoints".to_string(), Value::Number(0.0));
    props.insert("hardwareConcurrency".to_string(), Value::Number(4.0));
    props.insert("onLine".to_string(), Value::Bool(true));
    props.insert("cookieEnabled".to_string(), Value::Bool(false));
    props.insert("pdfViewerEnabled".to_string(), Value::Bool(false));
    props.insert("sendBeacon".to_string(), func(|_, _| Value::Bool(false)));
    props.insert("vibrate".to_string(), func(|_, _| Value::Bool(false)));

    let mut clipboard: HashMap<String, Value> = HashMap::new();
    clipboard.insert(
        "readText".to_string(),
        func(|_, _| resolved_thenable(Value::string(&clipboard_read_text()))),
    );
    clipboard.insert(
        "writeText".to_string(),
        func(|_, args| {
            clipboard_write_text(&arg(&args, 0).to_js_string());
            resolved_thenable(Value::Undefined)
        }),
    );
    clipboard.insert(
        "read".to_string(),
        func(|_, _| resolved_thenable(js_array(vec![]))),
    );
    clipboard.insert(
        "write".to_string(),
        func(|_, _| resolved_thenable(Value::Undefined)),
    );
    props.insert("clipboard".to_string(), Value::object(clipboard));

    Value::object(props)
}

fn location_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert("href".to_string(), Value::string("w3cos://app"));
    props.insert("origin".to_string(), Value::string("w3cos://app"));
    props.insert("protocol".to_string(), Value::string("w3cos:"));
    props.insert("host".to_string(), Value::string("app"));
    props.insert("hostname".to_string(), Value::string("app"));
    props.insert("port".to_string(), Value::string(""));
    props.insert("pathname".to_string(), Value::string("/"));
    props.insert("search".to_string(), Value::string(""));
    props.insert("hash".to_string(), Value::string(""));
    props.insert("ancestorOrigins".to_string(), js_array(vec![]));
    for name in ["assign", "replace", "reload"] {
        props.insert(name.to_string(), func(|_, _| Value::Undefined));
    }
    props.insert(
        "toString".to_string(),
        func(|_, _| Value::string("w3cos://app")),
    );
    Value::object(props)
}

fn performance_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "now".to_string(),
        func(|_, _| Value::Number(performance_now())),
    );
    props.insert("timeOrigin".to_string(), Value::Number(0.0));
    for name in ["mark", "measure", "clearMarks", "clearMeasures"] {
        props.insert(name.to_string(), func(|_, _| Value::Undefined));
    }
    for name in ["getEntries", "getEntriesByName", "getEntriesByType"] {
        props.insert(name.to_string(), func(|_, _| js_array(vec![])));
    }
    Value::object(props)
}

fn viewport() -> (f64, f64, f64) {
    VIEWPORT.with(|v| v.get())
}

/// Set the viewport size reported by `window.innerWidth/innerHeight`,
/// `screen`, and `matchMedia`. Default 1024x768.
pub fn set_viewport(width: f64, height: f64) {
    VIEWPORT.with(|v| {
        let (_, _, dpr) = v.get();
        v.set((width, height, dpr));
    });
}

/// Set the devicePixelRatio reported by the window. Default 1.0.
pub fn set_device_pixel_ratio(dpr: f64) {
    VIEWPORT.with(|v| {
        let (w, h, _) = v.get();
        v.set((w, h, dpr));
    });
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
            value.map(|v| Value::String(v)).unwrap_or(Value::Null)
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
                    .map(|k| Value::String(k))
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
    Value::object(props)
}

fn next_random() -> u64 {
    RNG_STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = (START_TIME.with(|t| t.elapsed().as_nanos()) as u64) | 0x9E3779B97F4A7C15;
        }
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        x.wrapping_mul(0x2545F4914F6CDD1D)
    })
}

fn crypto_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "getRandomValues".to_string(),
        func(|_, args| {
            let arr = arg(&args, 0);
            if let Value::Array(items) = &arr {
                let mut items = items.borrow_mut();
                for slot in items.iter_mut() {
                    *slot = Value::Number((next_random() & 0xff) as f64);
                }
            }
            arr
        }),
    );
    props.insert(
        "randomUUID".to_string(),
        func(|_, _| {
            let mut bytes = [0u8; 16];
            for chunk in bytes.chunks_mut(8) {
                let r = next_random().to_le_bytes();
                chunk.copy_from_slice(&r[..chunk.len()]);
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
    Value::object(props)
}

fn match_media_value(query: &str) -> Value {
    let matches = crate::media::parse_media_query(query)
        .map(|cond| {
            let (w, h, dpr) = viewport();
            crate::media::matches_media(
                &cond,
                &crate::media::Viewport::new(w as f32, h as f32, dpr as f32),
            )
        })
        .unwrap_or(false);
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert("matches".to_string(), Value::Bool(matches));
    props.insert("media".to_string(), Value::string(query));
    props.insert("onchange".to_string(), Value::Null);
    for name in [
        "addEventListener",
        "removeEventListener",
        "addListener",
        "removeListener",
    ] {
        props.insert(name.to_string(), func(|_, _| Value::Undefined));
    }
    props.insert("dispatchEvent".to_string(), func(|_, _| Value::Bool(false)));
    Value::object(props)
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
    props.insert("closed".to_string(), Value::Bool(false));
    props.insert("isSecureContext".to_string(), Value::Bool(true));
    props.insert("crossOriginIsolated".to_string(), Value::Bool(false));
    props.insert("origin".to_string(), Value::string("w3cos://app"));
    props.insert("name".to_string(), Value::string(""));
    props.insert("status".to_string(), Value::string(""));
    props.insert("length".to_string(), Value::Number(0.0));
    props.insert("frames".to_string(), js_array(vec![]));

    // history stub
    {
        let mut history: HashMap<String, Value> = HashMap::new();
        history.insert("length".to_string(), Value::Number(1.0));
        history.insert("state".to_string(), Value::Null);
        history.insert("scrollRestoration".to_string(), Value::string("auto"));
        for name in ["pushState", "replaceState", "back", "forward", "go"] {
            history.insert(name.to_string(), func(|_, _| Value::Undefined));
        }
        props.insert("history".to_string(), Value::object(history));
    }

    // screen
    {
        let mut screen: HashMap<String, Value> = HashMap::new();
        for key in ["width", "height", "availWidth", "availHeight"] {
            let height = key.contains("eight"); // height / availHeight
            screen.insert(
                format!("__w3cos_getter_{key}"),
                func(move |_, _| {
                    let (w, h, _) = viewport();
                    Value::Number(if height { h } else { w })
                }),
            );
        }
        screen.insert("colorDepth".to_string(), Value::Number(24.0));
        screen.insert("pixelDepth".to_string(), Value::Number(24.0));
        let mut orientation: HashMap<String, Value> = HashMap::new();
        orientation.insert("type".to_string(), Value::string("landscape-primary"));
        orientation.insert("angle".to_string(), Value::Number(0.0));
        screen.insert("orientation".to_string(), Value::object(orientation));
        props.insert("screen".to_string(), Value::object(screen));
    }

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
        vv.insert("offsetLeft".to_string(), Value::Number(0.0));
        vv.insert("offsetTop".to_string(), Value::Number(0.0));
        vv.insert("pageLeft".to_string(), Value::Number(0.0));
        vv.insert("pageTop".to_string(), Value::Number(0.0));
        for name in ["addEventListener", "removeEventListener"] {
            vv.insert(name.to_string(), func(|_, _| Value::Undefined));
        }
        props.insert("visualViewport".to_string(), Value::object(vv));
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
        props.insert(
            format!("__w3cos_getter_{key}"),
            func(|_, _| Value::Number(0.0)),
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
        func(|_, args| {
            // Cascade gap: returns the element's inline style declaration.
            match node_id_of(&arg(&args, 0)) {
                Some(node) => style_value(node),
                None => Value::object(HashMap::new()),
            }
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
            Value::Number(js_set_timer(cb, 0, vec![], false) as f64)
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
        "scrollTo", "scrollBy", "scroll", "moveTo", "moveBy", "resizeTo", "resizeBy", "focus",
        "blur", "print", "close", "stop",
    ] {
        props.insert(name.to_string(), func(|_, _| Value::Undefined));
    }
    props.insert("open".to_string(), func(|_, _| Value::Null));
    props.insert("alert".to_string(), func(|_, _| Value::Undefined));
    props.insert("confirm".to_string(), func(|_, _| Value::Bool(false)));
    props.insert("prompt".to_string(), func(|_, _| Value::Null));
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

    Value::object(props)
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

fn js_clear_timer(id: u32) {
    JS_TIMERS.with(|t| t.borrow_mut().retain(|timer| timer.id != id));
}

/// Fire all due `setTimeout`/`setInterval` callbacks and drain the
/// `requestAnimationFrame` queue. Returns the number of callbacks invoked.
/// The frame loop should call this once per frame (integration point:
/// `window.rs`, intentionally not wired by this module).
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
    let raf_callbacks: Vec<Value> =
        RAF_QUEUE.with(|q| q.borrow_mut().drain(..).map(|(_, cb)| cb).collect());
    let mut ran = fired.len();
    for (cb, args) in fired {
        cb.call(Value::Undefined, args);
    }
    if !raf_callbacks.is_empty() {
        let timestamp = Value::Number(performance_now());
        for cb in raf_callbacks {
            cb.call(Value::Undefined, vec![timestamp.clone()]);
            ran += 1;
        }
    }
    ran
}

/// Queue a microtask (also used internally for thenable callbacks).
pub fn queue_microtask_value(callback: Value) {
    if callback.is_function() {
        MICROTASKS.with(|m| m.borrow_mut().push(callback));
    }
}

/// Drain pending native event snapshots and the microtask queue (repeating
/// until both are empty, since handlers may enqueue more work). Returns the
/// total number of callbacks invoked. The frame loop should call this once
/// per frame. `w3cos_core::promise` reaction jobs are drained on every
/// iteration so promise callbacks interleave with bridge microtasks/events.
pub fn drain_microtasks() -> usize {
    let mut ran = 0;
    loop {
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

/// True when the bridge has work for the frame loop: pending JS timers,
/// rAF callbacks, microtasks, or undelivered native events.
pub fn has_pending_work() -> bool {
    let timers = JS_TIMERS.with(|t| !t.borrow().is_empty());
    let raf = RAF_QUEUE.with(|q| !q.borrow().is_empty());
    let micro = MICROTASKS.with(|m| !m.borrow().is_empty());
    let events = PENDING_EVENTS.with(|q| !q.borrow().is_empty());
    timers || raf || micro || events
}

/// Earliest deadline the bridge needs to be woken at: the soonest pending JS
/// timer, or the next frame (~16ms) when rAF callbacks are queued. The event
/// loop folds this into its `ControlFlow::WaitUntil` computation.
pub fn next_timer_deadline() -> Option<Instant> {
    let timer = JS_TIMERS.with(|t| t.borrow().iter().map(|timer| timer.fire_at).min());
    let raf = RAF_QUEUE.with(|q| !q.borrow().is_empty());
    match (timer, raf) {
        (Some(deadline), true) => Some(deadline.min(Instant::now() + Duration::from_millis(16))),
        (Some(deadline), false) => Some(deadline),
        (None, true) => Some(Instant::now() + Duration::from_millis(16)),
        (None, false) => None,
    }
}

/// Reset all bridge state. Pair with [`crate::dom::reset_document`] in tests:
/// node ids are recycled by a fresh document, so the element-value memo and
/// every other node-keyed cache must be dropped too.
pub fn reset_bridge() {
    ELEMENT_VALUES.with(|c| c.borrow_mut().clear());
    ELEMENT_PROPS.with(|c| c.borrow_mut().clear());
    STYLE_CACHE.with(|c| c.borrow_mut().clear());
    LISTENERS.with(|l| l.borrow_mut().clear());
    NATIVELY_REGISTERED.with(|r| r.borrow_mut().clear());
    PENDING_EVENTS.with(|q| q.borrow_mut().clear());
    CUSTOM_EVENT_TYPES.with(|m| m.borrow_mut().clear());
    CUSTOM_EVENT_NAMES.with(|m| m.borrow_mut().clear());
    MICROTASKS.with(|m| m.borrow_mut().clear());
    JS_TIMERS.with(|t| t.borrow_mut().clear());
    NEXT_TIMER_ID.with(|c| c.set(1));
    RAF_QUEUE.with(|q| q.borrow_mut().clear());
    NEXT_RAF_ID.with(|c| c.set(1));
    VIEWPORT.with(|v| v.set((1024.0, 768.0, 1.0)));
    ACTIVE_ELEMENT.with(|a| *a.borrow_mut() = None);
    HTML_ID.with(|h| *h.borrow_mut() = None);
    HEAD_ID.with(|h| *h.borrow_mut() = None);
    CANVAS_CONTEXTS.with(|c| c.borrow_mut().clear());
    SESSION_STORAGE.with(|s| s.borrow_mut().clear());
    // DOCUMENT_VALUE / WINDOW_VALUE / SELECTION_VALUE survive on purpose:
    // their contents read all state lazily from the DOM and viewport.
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
    }

    fn create_in_body(tag: &str) -> Value {
        let doc = document_value();
        let el = doc.call_method("createElement", vec![Value::string(tag)]);
        doc.get_property("body")
            .call_method("appendChild", vec![el.clone()]);
        el
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
        let ev = Value::object(ev_props);
        let not_canceled = child.call_method("dispatchEvent", vec![ev]).to_bool();
        assert!(not_canceled);
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
        let mut ev_props = HashMap::new();
        ev_props.insert("type".to_string(), Value::string("cancel-me"));
        let canceled = !child
            .call_method("dispatchEvent", vec![Value::object(ev_props)])
            .to_bool();
        assert!(canceled);
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
        let got_ts = Rc::new(Cell::new(-1.0f64));
        let got_ts2 = got_ts.clone();
        win.call_method(
            "requestAnimationFrame",
            vec![func(move |_, args| {
                got_ts2.set(arg(&args, 0).to_number());
                Value::Undefined
            })],
        );
        assert_eq!(tick_timers(), 1);
        assert!(got_ts.get() >= 0.0);
        assert_eq!(tick_timers(), 0);
    }

    #[test]
    fn microtasks_and_thenables() {
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
    fn window_viewport_and_match_media() {
        setup();
        let win = window_value();
        assert_eq!(win.get_property("innerWidth").to_number(), 1024.0);
        assert_eq!(win.get_property("innerHeight").to_number(), 768.0);
        assert_eq!(win.get_property("devicePixelRatio").to_number(), 1.0);
        set_viewport(1440.0, 900.0);
        set_device_pixel_ratio(2.0);
        assert_eq!(win.get_property("innerWidth").to_number(), 1440.0);
        assert_eq!(win.get_property("devicePixelRatio").to_number(), 2.0);

        let mql = win.call_method("matchMedia", vec![Value::string("(min-width: 600px)")]);
        assert!(mql.get_property("matches").to_bool());
        let mql2 = win.call_method("matchMedia", vec![Value::string("(max-width: 600px)")]);
        assert!(!mql2.get_property("matches").to_bool());
    }

    #[test]
    fn window_document_and_self_references() {
        setup();
        let win = window_value();
        let doc = win.get_property("document");
        assert!(doc == document_value());
        assert!(win.get_property("self") == win);
        assert!(win.get_property("window") == win);
        assert!(doc.get_property("defaultView") == win);
        let nav = win.get_property("navigator");
        assert_eq!(nav.get_property("maxTouchPoints").to_number(), 0.0);
        assert_eq!(nav.get_property("language").to_js_string(), "en-US");
        let loc = win.get_property("location");
        assert_eq!(loc.get_property("href").to_js_string(), "w3cos://app");
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
        assert!(ctx == canvas.call_method("getContext", vec![Value::string("2d")]));
        ctx.set_property("fillStyle", Value::string("#ff0000"));
        assert_eq!(ctx.get_property("fillStyle").to_js_string(), "#ff0000");
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
        // Non-2d contexts are unsupported.
        assert!(
            canvas
                .call_method("getContext", vec![Value::string("webgl")])
                .is_null()
        );
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

    fn win_selection() -> Value {
        window_value().call_method("getSelection", vec![])
    }

    #[test]
    fn bounding_client_rect_zeroes_until_layout() {
        setup();
        let div = create_in_body("div");
        let rect = div.call_method("getBoundingClientRect", vec![]);
        for key in [
            "x", "y", "width", "height", "top", "left", "right", "bottom",
        ] {
            assert_eq!(rect.get_property(key).to_number(), 0.0, "{key}");
        }
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
        let cs = win.call_method("getComputedStyle", vec![div]);
        assert_eq!(cs.get_property("width").to_js_string(), "42px");
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
}
