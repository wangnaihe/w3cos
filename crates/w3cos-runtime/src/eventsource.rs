//! W3C EventSource API — Server-Sent Events (SSE)
//!
//! Mirrors the WHATWG EventSource interface:
//! https://html.spec.whatwg.org/multipage/server-sent-events.html
//!
//! SSE is the standard transport for LLM streaming responses (OpenAI, Anthropic,
//! etc. all use `text/event-stream`). This implementation runs the HTTP
//! connection on a background thread and surfaces events via `poll_events()`
//! for use in a frame loop, matching the pattern used by `WebSocket`.
//!
//! # Example — OpenAI streaming chat
//! ```ignore
//! let es = EventSource::new("https://api.openai.com/v1/chat/completions")
//!     .with_header("Authorization", "Bearer sk-...")
//!     .with_header("Content-Type", "application/json")
//!     .with_method("POST")
//!     .with_body(r#"{"model":"gpt-4o","stream":true,"messages":[...]}"#)
//!     .connect();
//!
//! // In frame loop:
//! for event in es.poll_events() {
//!     match event {
//!         SseEvent::Message { data, .. } => handle_token(data),
//!         SseEvent::Error(e) => log(e),
//!         SseEvent::Close => break,
//!     }
//! }
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use w3cos_core::Value;

// ── ReadyState ─────────────────────────────────────────────────────────────

/// `EventSource.readyState` — matches the W3C numeric values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventSourceState {
    Connecting = 0,
    Open = 1,
    Closed = 2,
}

impl EventSourceState {
    fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::Connecting,
            1 => Self::Open,
            _ => Self::Closed,
        }
    }
}

// ── SseEvent ───────────────────────────────────────────────────────────────

/// An event dispatched by the `EventSource`.
#[derive(Debug, Clone)]
pub enum SseEvent {
    /// `onopen` — connection established and first byte received.
    Open,
    /// `onmessage` — a complete SSE event parsed from the stream.
    Message {
        /// The `event:` field, defaults to `"message"`.
        event: String,
        /// The `data:` field (may be multi-line, joined with `\n`).
        data: String,
        /// The `id:` field, if present.
        id: Option<String>,
        /// The `retry:` field in milliseconds, if present.
        retry: Option<u64>,
    },
    /// `onerror` — a transport or parse error.
    Error(String),
    /// Connection closed (server sent `data: [DONE]` or EOF).
    Close,
}

// ── Builder ────────────────────────────────────────────────────────────────

/// Builder for an `EventSource` connection.
/// Supports both GET (standard SSE) and POST (LLM streaming APIs).
pub struct EventSourceBuilder {
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body: Option<String>,
}

impl EventSourceBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: "GET".into(),
            headers: HashMap::new(),
            body: None,
        }
    }

    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Open the connection and return an `EventSource` handle.
    pub fn connect(self) -> EventSource {
        EventSource::connect_inner(self)
    }
}

// ── EventSource ────────────────────────────────────────────────────────────

struct EventSourceInner {
    url: String,
    state: AtomicU8,
    events: Mutex<VecDeque<SseEvent>>,
    last_event_id: Mutex<Option<String>>,
}

/// W3C `EventSource` handle.
///
/// Cloning is cheap (`Arc` internally). Call `poll_events()` from a frame
/// loop to drain pending events without blocking the UI thread.
pub struct EventSource {
    inner: Arc<EventSourceInner>,
}

impl EventSource {
    /// `new EventSource(url)` — standard GET-based SSE.
    pub fn new(url: impl Into<String>) -> Self {
        EventSourceBuilder::new(url).connect()
    }

    /// Start a builder for advanced configuration (POST body, custom headers).
    pub fn builder(url: impl Into<String>) -> EventSourceBuilder {
        EventSourceBuilder::new(url)
    }

    fn connect_inner(builder: EventSourceBuilder) -> Self {
        let inner = Arc::new(EventSourceInner {
            url: builder.url.clone(),
            state: AtomicU8::new(EventSourceState::Connecting as u8),
            events: Mutex::new(VecDeque::new()),
            last_event_id: Mutex::new(None),
        });

        let worker_inner = Arc::clone(&inner);
        thread::Builder::new()
            .name(format!("w3cos-sse-{}", builder.url))
            .spawn(move || sse_worker(worker_inner, builder))
            .expect("spawn EventSource worker");

        EventSource { inner }
    }

    /// `EventSource.readyState`
    pub fn ready_state(&self) -> EventSourceState {
        EventSourceState::from_u8(self.inner.state.load(Ordering::SeqCst))
    }

    /// `EventSource.url`
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// The last received `id:` field value (`EventSource.lastEventId`).
    pub fn last_event_id(&self) -> Option<String> {
        self.inner.last_event_id.lock().unwrap().clone()
    }

    /// Drain all pending events. Call from a frame loop.
    pub fn poll_events(&self) -> Vec<SseEvent> {
        let mut q = self.inner.events.lock().unwrap();
        q.drain(..).collect()
    }

    /// `EventSource.close()` — stop reconnecting and close the connection.
    pub fn close(&self) {
        self.inner
            .state
            .store(EventSourceState::Closed as u8, Ordering::SeqCst);
    }
}

impl Clone for EventSource {
    fn clone(&self) -> Self {
        EventSource {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[derive(Clone)]
struct JsEventSource {
    source: EventSource,
    value: Value,
    listeners: Rc<RefCell<HashMap<String, Vec<Value>>>>,
}

thread_local! {
    static JS_EVENT_SOURCES: RefCell<Vec<JsEventSource>> = const { RefCell::new(Vec::new()) };
    static EVENT_SOURCE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn event_source_class() -> Value {
    EVENT_SOURCE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let url = args.first().cloned().unwrap_or_default().to_js_string();
            js_event_source_value(EventSource::new(url))
        });
        for (name, state) in [
            ("CONNECTING", EventSourceState::Connecting),
            ("OPEN", EventSourceState::Open),
            ("CLOSED", EventSourceState::Closed),
        ] {
            class.set_property(name, Value::Number(state as u8 as f64));
        }
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn js_event_source_value(source: EventSource) -> Value {
    let listeners = Rc::new(RefCell::new(HashMap::<String, Vec<Value>>::new()));
    let value = Value::object(HashMap::from([
        ("onopen".to_string(), Value::Null),
        ("onmessage".to_string(), Value::Null),
        ("onerror".to_string(), Value::Null),
        ("withCredentials".to_string(), Value::Bool(false)),
    ]));
    let source_for_url = source.clone();
    value.set_property(
        "__w3cos_getter_url",
        Value::function(move |_, _| Value::string(source_for_url.url())),
    );
    let source_for_state = source.clone();
    value.set_property(
        "__w3cos_getter_readyState",
        Value::function(move |_, _| Value::Number(source_for_state.ready_state() as u8 as f64)),
    );
    let source_for_close = source.clone();
    value.set_property(
        "close",
        Value::function(move |_, _| {
            source_for_close.close();
            Value::Undefined
        }),
    );
    let add_listeners = Rc::clone(&listeners);
    value.set_property(
        "addEventListener",
        Value::function(move |_, args| {
            let event_type = args.first().cloned().unwrap_or_default().to_js_string();
            let listener = args.get(1).cloned().unwrap_or_default();
            if listener.is_function() {
                add_listeners
                    .borrow_mut()
                    .entry(event_type)
                    .or_default()
                    .push(listener);
            }
            Value::Undefined
        }),
    );
    let remove_listeners = Rc::clone(&listeners);
    value.set_property(
        "removeEventListener",
        Value::function(move |_, args| {
            let event_type = args.first().cloned().unwrap_or_default().to_js_string();
            let listener = args.get(1).cloned().unwrap_or_default();
            if let Some(items) = remove_listeners.borrow_mut().get_mut(&event_type) {
                items.retain(|item| !item.same_value_zero(&listener));
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &event_source_class().get_property("prototype"));
    JS_EVENT_SOURCES.with(|sources| {
        sources.borrow_mut().push(JsEventSource {
            source,
            value: value.clone(),
            listeners,
        });
    });
    value
}

fn dispatch_js_event(binding: &JsEventSource, event_type: &str, event: Value) -> usize {
    event.set_property("target", binding.value.clone());
    event.set_property("currentTarget", binding.value.clone());
    let mut callbacks = binding
        .listeners
        .borrow()
        .get(event_type)
        .cloned()
        .unwrap_or_default();
    let property = binding.value.get_property(&format!("on{event_type}"));
    if property.is_function() {
        callbacks.insert(0, property);
    }
    for callback in &callbacks {
        callback.call(binding.value.clone(), vec![event.clone()]);
    }
    callbacks.len()
}

pub fn poll_js_events() -> usize {
    let bindings = JS_EVENT_SOURCES.with(|sources| sources.borrow().clone());
    let mut dispatched = 0;
    for binding in bindings {
        for event in binding.source.poll_events() {
            let (event_type, value) = match event {
                SseEvent::Open => ("open".to_string(), Value::Undefined),
                SseEvent::Message {
                    event, data, id, ..
                } => (
                    event,
                    Value::object(HashMap::from([
                        ("data".to_string(), Value::string(&data)),
                        (
                            "lastEventId".to_string(),
                            id.map(|id| Value::string(&id)).unwrap_or(Value::string("")),
                        ),
                        ("origin".to_string(), Value::string("")),
                    ])),
                ),
                SseEvent::Error(message) => ("error".to_string(), Value::string(&message)),
                SseEvent::Close => ("error".to_string(), Value::Undefined),
            };
            let event_value = w3cos_core::class::construct(
                &crate::web_events::event_class(),
                vec![Value::string(&event_type)],
            );
            if !value.is_undefined() {
                if value.is_object() {
                    for key in ["data", "lastEventId", "origin"] {
                        let property = value.get_property(key);
                        if !property.is_undefined() {
                            event_value.set_property(key, property);
                        }
                    }
                } else {
                    event_value.set_property("message", value);
                }
            }
            dispatched += dispatch_js_event(&binding, &event_type, event_value);
        }
    }
    dispatched
}

// ── SSE worker thread ──────────────────────────────────────────────────────

fn push(inner: &Arc<EventSourceInner>, event: SseEvent) {
    if let Ok(mut q) = inner.events.lock() {
        q.push_back(event);
    }
}

fn is_closed(inner: &Arc<EventSourceInner>) -> bool {
    inner.state.load(Ordering::SeqCst) == EventSourceState::Closed as u8
}

fn sse_worker(inner: Arc<EventSourceInner>, builder: EventSourceBuilder) {
    // Build ureq request
    let config = ureq::Agent::config_builder()
        .timeout_global(None) // SSE streams are long-lived
        .build();
    let agent = ureq::Agent::new_with_config(config);

    // Build ureq request — always use the body-capable path to unify types.
    // For GET we send an empty body via call(); for POST/PUT we send the actual body.
    let resp_result = if let Some(ref body) = builder.body {
        let mut req = match builder.method.to_uppercase().as_str() {
            "PUT" => agent.put(&builder.url),
            _ => agent.post(&builder.url),
        };
        req = req.header("Accept", "text/event-stream");
        req = req.header("Cache-Control", "no-cache");
        for (k, v) in &builder.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        req.send(body.as_bytes())
    } else {
        // GET (and any other method) — use ureq's get() and call()
        let mut req = agent.get(&builder.url);
        req = req.header("Accept", "text/event-stream");
        req = req.header("Cache-Control", "no-cache");
        for (k, v) in &builder.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        req.call()
    };

    let resp: ureq::http::Response<ureq::Body> = match resp_result {
        Ok(r) => r,
        Err(e) => {
            push(&inner, SseEvent::Error(format!("connect failed: {e}")));
            push(&inner, SseEvent::Close);
            inner
                .state
                .store(EventSourceState::Closed as u8, Ordering::SeqCst);
            return;
        }
    };

    inner
        .state
        .store(EventSourceState::Open as u8, Ordering::SeqCst);
    push(&inner, SseEvent::Open);

    // Parse SSE line-by-line
    let mut body = resp.into_body();
    let reader = BufReader::new(body.as_reader());
    let mut event_type = String::from("message");
    let mut data_lines: Vec<String> = Vec::new();
    let mut event_id: Option<String> = None;
    let mut retry: Option<u64> = None;

    for line_result in reader.lines() {
        if is_closed(&inner) {
            break;
        }

        let line: String = match line_result {
            Ok(l) => l,
            Err(e) => {
                push(&inner, SseEvent::Error(format!("read error: {e}")));
                break;
            }
        };

        if line.is_empty() {
            // Empty line = dispatch event if we have data
            if !data_lines.is_empty() {
                let data = data_lines.join("\n");

                // OpenAI / Anthropic sentinel
                if data == "[DONE]" {
                    push(&inner, SseEvent::Close);
                    inner
                        .state
                        .store(EventSourceState::Closed as u8, Ordering::SeqCst);
                    break;
                }

                if let Some(ref id) = event_id {
                    *inner.last_event_id.lock().unwrap() = Some(id.clone());
                }

                push(
                    &inner,
                    SseEvent::Message {
                        event: event_type.clone(),
                        data,
                        id: event_id.clone(),
                        retry,
                    },
                );

                // Reset per-event fields
                event_type = "message".into();
                data_lines.clear();
                event_id = None;
                retry = None;
            }
            continue;
        }

        // Comment line
        if line.starts_with(':') {
            continue;
        }

        // Field parsing
        let (field, value) = if let Some(pos) = line.find(':') {
            let f = &line[..pos];
            let v = line[pos + 1..].trim_start_matches(' ');
            (f, v.to_string())
        } else {
            (line.as_str(), String::new())
        };

        match field {
            "data" => data_lines.push(value),
            "event" => event_type = value,
            "id" => event_id = Some(value),
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    retry = Some(ms);
                }
            }
            _ => {}
        }
    }

    // EOF
    if !is_closed(&inner) {
        push(&inner, SseEvent::Close);
        inner
            .state
            .store(EventSourceState::Closed as u8, Ordering::SeqCst);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse SSE lines manually to verify the field parser logic.
    #[test]
    fn parse_sse_fields() {
        // Simulate what sse_worker does with a sequence of lines
        let lines = vec![
            "data: hello",
            "event: token",
            "id: 42",
            "",
            "data: world",
            "",
        ];

        let mut event_type = "message".to_string();
        let mut data_lines: Vec<String> = Vec::new();
        let mut event_id: Option<String> = None;
        let mut events: Vec<(String, String, Option<String>)> = Vec::new();

        for line in lines {
            if line.is_empty() {
                if !data_lines.is_empty() {
                    events.push((event_type.clone(), data_lines.join("\n"), event_id.clone()));
                    event_type = "message".into();
                    data_lines.clear();
                    event_id = None;
                }
                continue;
            }
            if let Some(pos) = line.find(':') {
                let field = &line[..pos];
                let value = line[pos + 1..].trim_start_matches(' ').to_string();
                match field {
                    "data" => data_lines.push(value),
                    "event" => event_type = value,
                    "id" => event_id = Some(value),
                    _ => {}
                }
            }
        }

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "token");
        assert_eq!(events[0].1, "hello");
        assert_eq!(events[0].2, Some("42".into()));
        assert_eq!(events[1].0, "message");
        assert_eq!(events[1].1, "world");
    }

    #[test]
    fn ready_state_initial() {
        // We can't easily test a live HTTP connection in unit tests,
        // but we can verify the initial state and close behavior.
        let es = EventSource::new("http://127.0.0.1:1"); // unreachable port
        // State is Connecting initially (may transition quickly to Closed)
        let s = es.ready_state();
        assert!(matches!(
            s,
            EventSourceState::Connecting | EventSourceState::Closed
        ));
        es.close();
        assert_eq!(es.ready_state(), EventSourceState::Closed);
    }
}
