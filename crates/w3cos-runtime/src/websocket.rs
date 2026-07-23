//! W3C WebSocket API — RFC 6455 client over `tungstenite`.
//!
//! Mirrors the browser `WebSocket` interface:
//!
//! ```text
//! const ws = new WebSocket("ws://localhost:9001");
//! ws.onopen = () => ws.send("hello");
//! ws.onmessage = (ev) => console.log(ev.data);
//! ws.onclose = () => console.log("closed");
//! ```
//!
//! In W3C OS the API is exposed via [`WebSocket::connect`] — the returned
//! handle hides the worker thread, exposes ready-state, allows
//! [`WebSocket::send_text`] / [`WebSocket::send_binary`] / [`WebSocket::close`],
//! and surfaces incoming events through a non-blocking poll
//! ([`WebSocket::poll_events`]). Reactive applications can call `poll_events`
//! from their frame loop and treat each [`WebSocketEvent`] as if dispatched
//! through `addEventListener`.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::net::TcpStream;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::Message;
use tungstenite::stream::MaybeTlsStream;
use w3cos_core::Value;

/// `WebSocket.readyState` — matches the W3C numeric values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReadyState {
    Connecting = 0,
    Open = 1,
    Closing = 2,
    Closed = 3,
}

impl ReadyState {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(n: u8) -> Self {
        match n {
            0 => ReadyState::Connecting,
            1 => ReadyState::Open,
            2 => ReadyState::Closing,
            _ => ReadyState::Closed,
        }
    }
}

/// Dispatched events — matches the W3C `MessageEvent` / `CloseEvent` /
/// `Event` model collapsed into one enum for `poll_events()`.
#[derive(Debug, Clone)]
pub enum WebSocketEvent {
    /// `onopen` — connection negotiated.
    Open,
    /// `onmessage` with a text payload.
    Text(String),
    /// `onmessage` with a binary payload (`event.data` as a `Uint8Array`).
    Binary(Vec<u8>),
    /// `onerror` — a transport or protocol failure occurred.
    Error(String),
    /// `onclose` — connection closed (cleanly when `was_clean`).
    Close {
        code: u16,
        reason: String,
        was_clean: bool,
    },
}

/// Outbound command sent from the application thread to the worker thread.
enum OutboundCommand {
    Send(Message),
    Close { code: u16, reason: String },
}

/// Handle to a live WebSocket connection.
///
/// Cloning the handle is cheap (`Arc` internally) and lets multiple
/// signals/components share the same socket safely.
pub struct WebSocket {
    inner: Arc<WebSocketInner>,
}

struct WebSocketInner {
    url: String,
    state: AtomicU8,
    buffered: AtomicU32,
    cmd_tx: Mutex<Option<mpsc::Sender<OutboundCommand>>>,
    events: Mutex<VecDeque<WebSocketEvent>>,
}

impl WebSocket {
    /// `new WebSocket(url)` — opens a connection asynchronously.
    /// Returns immediately; check [`Self::ready_state`] or
    /// drain [`Self::poll_events`] to react to `Open` / `Error`.
    pub fn connect(url: impl Into<String>) -> Self {
        let url = url.into();
        let (cmd_tx, cmd_rx) = mpsc::channel::<OutboundCommand>();

        let inner = Arc::new(WebSocketInner {
            url: url.clone(),
            state: AtomicU8::new(ReadyState::Connecting.as_u8()),
            buffered: AtomicU32::new(0),
            cmd_tx: Mutex::new(Some(cmd_tx)),
            events: Mutex::new(VecDeque::new()),
        });

        let worker_inner = Arc::clone(&inner);
        thread::Builder::new()
            .name(format!("w3cos-ws-{url}"))
            .spawn(move || worker_loop(worker_inner, cmd_rx))
            .expect("spawn websocket worker");

        WebSocket { inner }
    }

    /// `WebSocket.url` — the remote endpoint.
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// `WebSocket.readyState` — current connection state.
    pub fn ready_state(&self) -> ReadyState {
        ReadyState::from_u8(self.inner.state.load(Ordering::SeqCst))
    }

    /// `WebSocket.bufferedAmount` — bytes queued but not yet sent.
    /// Approximated as the number of pending outbound messages.
    pub fn buffered_amount(&self) -> u32 {
        self.inner.buffered.load(Ordering::SeqCst)
    }

    /// `WebSocket.send(string)` — queue a text frame for transmission.
    pub fn send_text(&self, payload: impl Into<String>) -> Result<(), String> {
        if self.ready_state() != ReadyState::Open {
            return Err("WebSocket is not open".to_string());
        }
        let payload = payload.into();
        let len = payload.len();
        self.send_command(
            OutboundCommand::Send(Message::Text(payload.into())),
            len as u32,
        )
    }

    /// `WebSocket.send(buffer)` — queue a binary frame.
    pub fn send_binary(&self, payload: Vec<u8>) -> Result<(), String> {
        if self.ready_state() != ReadyState::Open {
            return Err("WebSocket is not open".to_string());
        }
        let len = payload.len();
        self.send_command(
            OutboundCommand::Send(Message::Binary(payload.into())),
            len as u32,
        )
    }

    /// `WebSocket.close([code[, reason]])` — initiate a clean close.
    pub fn close(&self, code: u16, reason: impl Into<String>) -> Result<(), String> {
        if matches!(self.ready_state(), ReadyState::Closing | ReadyState::Closed) {
            return Ok(());
        }
        // Mark as Closing so callers see the transition before the worker exits.
        self.inner
            .state
            .store(ReadyState::Closing.as_u8(), Ordering::SeqCst);
        self.send_command(
            OutboundCommand::Close {
                code,
                reason: reason.into(),
            },
            0,
        )
    }

    /// Drain pending events (consume from the worker queue).
    ///
    /// Idiomatic usage in a frame loop:
    ///
    /// ```ignore
    /// for ev in ws.poll_events() {
    ///     match ev {
    ///         WebSocketEvent::Open => log("connected"),
    ///         WebSocketEvent::Text(t) => append(t),
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub fn poll_events(&self) -> Vec<WebSocketEvent> {
        let mut q = self.inner.events.lock().expect("websocket mutex poisoned");
        q.drain(..).collect()
    }

    fn send_command(&self, cmd: OutboundCommand, queued_bytes: u32) -> Result<(), String> {
        let guard = self.inner.cmd_tx.lock().expect("websocket mutex poisoned");
        let tx = guard
            .as_ref()
            .ok_or_else(|| "WebSocket already closed".to_string())?;
        self.inner
            .buffered
            .fetch_add(queued_bytes, Ordering::SeqCst);
        if let Err(error) = tx.send(cmd) {
            self.inner
                .buffered
                .fetch_sub(queued_bytes, Ordering::SeqCst);
            return Err(format!("WebSocket send failed: {error}"));
        }
        Ok(())
    }
}

impl Clone for WebSocket {
    fn clone(&self) -> Self {
        WebSocket {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[derive(Clone)]
struct JsWebSocket {
    socket: WebSocket,
    value: Value,
    listeners: Rc<RefCell<HashMap<String, Vec<Value>>>>,
}

thread_local! {
    static JS_WEBSOCKETS: RefCell<Vec<JsWebSocket>> = const { RefCell::new(Vec::new()) };
}

/// Standard JavaScript `WebSocket` constructor used by the ESM runtime.
///
/// The native worker owns transport I/O while [`poll_js_events`] delivers
/// browser-shaped events on the JavaScript/application thread.
pub fn websocket_class() -> Value {
    let constructor = Value::function(|_, args| {
        let url = args
            .first()
            .cloned()
            .unwrap_or(Value::Undefined)
            .to_js_string();
        js_websocket_value(WebSocket::connect(url))
    });
    for (name, state) in [
        ("CONNECTING", ReadyState::Connecting),
        ("OPEN", ReadyState::Open),
        ("CLOSING", ReadyState::Closing),
        ("CLOSED", ReadyState::Closed),
    ] {
        constructor.set_property(name, Value::Number(state.as_u8() as f64));
    }
    constructor
}

fn js_websocket_value(socket: WebSocket) -> Value {
    let listeners: Rc<RefCell<HashMap<String, Vec<Value>>>> = Rc::new(RefCell::new(HashMap::new()));
    let mut props = HashMap::new();

    for name in ["open", "message", "error", "close"] {
        props.insert(format!("on{name}"), Value::Null);
    }
    props.insert("binaryType".to_string(), Value::from("blob"));
    props.insert("extensions".to_string(), Value::from(""));
    props.insert("protocol".to_string(), Value::from(""));

    for (name, state) in [
        ("CONNECTING", ReadyState::Connecting),
        ("OPEN", ReadyState::Open),
        ("CLOSING", ReadyState::Closing),
        ("CLOSED", ReadyState::Closed),
    ] {
        props.insert(name.to_string(), Value::Number(state.as_u8() as f64));
    }

    let socket_for_url = socket.clone();
    props.insert(
        "__w3cos_getter_url".to_string(),
        Value::function(move |_, _| Value::from(socket_for_url.url())),
    );
    let socket_for_state = socket.clone();
    props.insert(
        "__w3cos_getter_readyState".to_string(),
        Value::function(move |_, _| Value::Number(socket_for_state.ready_state().as_u8() as f64)),
    );
    let socket_for_buffered = socket.clone();
    props.insert(
        "__w3cos_getter_bufferedAmount".to_string(),
        Value::function(move |_, _| Value::Number(socket_for_buffered.buffered_amount() as f64)),
    );

    let socket_for_send = socket.clone();
    props.insert(
        "send".to_string(),
        Value::function(move |this, args| {
            let payload = args.first().cloned().unwrap_or(Value::Undefined);
            let result = if let Value::Array(items) = payload {
                socket_for_send.send_binary(
                    items
                        .borrow()
                        .iter()
                        .map(|item| item.to_u32() as u8)
                        .collect(),
                )
            } else {
                socket_for_send.send_text(payload.to_js_string())
            };
            if let Err(message) = result {
                dispatch_js_event(&this, "error", Some(Value::from(message)), None);
            }
            Value::Undefined
        }),
    );
    let socket_for_close = socket.clone();
    props.insert(
        "close".to_string(),
        Value::function(move |_, args| {
            let code = args
                .first()
                .filter(|value| !value.is_undefined())
                .map(Value::to_u32)
                .unwrap_or(1000) as u16;
            let reason = args
                .get(1)
                .filter(|value| !value.is_undefined())
                .map(Value::to_js_string)
                .unwrap_or_default();
            let _ = socket_for_close.close(code, reason);
            Value::Undefined
        }),
    );

    let listeners_for_add = Rc::clone(&listeners);
    props.insert(
        "addEventListener".to_string(),
        Value::function(move |_, args| {
            let event_type = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
            if handler.is_function() {
                listeners_for_add
                    .borrow_mut()
                    .entry(event_type)
                    .or_default()
                    .push(handler);
            }
            Value::Undefined
        }),
    );
    let listeners_for_remove = Rc::clone(&listeners);
    props.insert(
        "removeEventListener".to_string(),
        Value::function(move |_, args| {
            let event_type = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
            if let Some(handlers) = listeners_for_remove.borrow_mut().get_mut(&event_type) {
                handlers.retain(|candidate| !candidate.same_value_zero(&handler));
            }
            Value::Undefined
        }),
    );
    let listeners_for_dispatch = Rc::clone(&listeners);
    props.insert(
        "dispatchEvent".to_string(),
        Value::function(move |this, args| {
            let event = args.first().cloned().unwrap_or(Value::Undefined);
            let event_type = event.get_property("type").to_js_string();
            dispatch_to_handlers(&this, &listeners_for_dispatch, &event_type, event);
            Value::Bool(true)
        }),
    );

    let value = Value::object(props);
    JS_WEBSOCKETS.with(|bindings| {
        bindings.borrow_mut().push(JsWebSocket {
            socket,
            value: value.clone(),
            listeners,
        })
    });
    value
}

fn dispatch_to_handlers(
    target: &Value,
    listeners: &Rc<RefCell<HashMap<String, Vec<Value>>>>,
    event_type: &str,
    event: Value,
) -> usize {
    event.set_property("target", target.clone());
    event.set_property("currentTarget", target.clone());
    let mut handlers = listeners
        .borrow()
        .get(event_type)
        .cloned()
        .unwrap_or_default();
    let property_handler = target.get_property(&format!("on{event_type}"));
    if property_handler.is_function() {
        handlers.insert(0, property_handler);
    }
    let count = handlers.len();
    for handler in handlers {
        handler.call(target.clone(), vec![event.clone()]);
    }
    count
}

fn dispatch_js_event(
    target: &Value,
    event_type: &str,
    data: Option<Value>,
    close: Option<(u16, String, bool)>,
) -> usize {
    let mut props = HashMap::from([
        ("type".to_string(), Value::from(event_type)),
        ("target".to_string(), target.clone()),
        ("currentTarget".to_string(), target.clone()),
    ]);
    if let Some(data) = data {
        if event_type == "error" {
            props.insert("message".to_string(), data);
        } else {
            props.insert("data".to_string(), data);
        }
    }
    if let Some((code, reason, was_clean)) = close {
        props.insert("code".to_string(), Value::Number(code as f64));
        props.insert("reason".to_string(), Value::from(reason));
        props.insert("wasClean".to_string(), Value::Bool(was_clean));
    }
    let event = Value::object(props);
    let listeners = JS_WEBSOCKETS.with(|bindings| {
        bindings
            .borrow()
            .iter()
            .find(|binding| binding.value.same_value_zero(target))
            .map(|binding| Rc::clone(&binding.listeners))
    });
    listeners
        .map(|listeners| dispatch_to_handlers(target, &listeners, event_type, event))
        .unwrap_or(0)
}

/// Deliver queued native WebSocket events to JavaScript handlers.
pub fn poll_js_events() -> usize {
    let bindings = JS_WEBSOCKETS.with(|bindings| bindings.borrow().clone());
    let mut delivered = 0;
    for binding in &bindings {
        for event in binding.socket.poll_events() {
            delivered += match event {
                WebSocketEvent::Open => dispatch_js_event(&binding.value, "open", None, None),
                WebSocketEvent::Text(text) => {
                    dispatch_js_event(&binding.value, "message", Some(Value::from(text)), None)
                }
                WebSocketEvent::Binary(bytes) => dispatch_js_event(
                    &binding.value,
                    "message",
                    Some(Value::array(
                        bytes
                            .into_iter()
                            .map(|byte| Value::Number(byte as f64))
                            .collect(),
                    )),
                    None,
                ),
                WebSocketEvent::Error(message) => {
                    dispatch_js_event(&binding.value, "error", Some(Value::from(message)), None)
                }
                WebSocketEvent::Close {
                    code,
                    reason,
                    was_clean,
                } => dispatch_js_event(
                    &binding.value,
                    "close",
                    None,
                    Some((code, reason, was_clean)),
                ),
            };
        }
    }
    JS_WEBSOCKETS.with(|items| {
        items
            .borrow_mut()
            .retain(|binding| binding.socket.ready_state() != ReadyState::Closed)
    });
    delivered
}

/// Whether an active JavaScript WebSocket needs periodic event-loop polling.
pub fn has_pending_js_sockets() -> bool {
    JS_WEBSOCKETS.with(|bindings| !bindings.borrow().is_empty())
}

pub(crate) fn reset_js_websockets() {
    JS_WEBSOCKETS.with(|bindings| bindings.borrow_mut().clear());
}

fn worker_loop(inner: Arc<WebSocketInner>, cmd_rx: mpsc::Receiver<OutboundCommand>) {
    let request = match (&inner.url[..]).into_client_request() {
        Ok(req) => req,
        Err(e) => {
            push_event(&inner, WebSocketEvent::Error(format!("invalid URL: {e}")));
            push_event(
                &inner,
                WebSocketEvent::Close {
                    code: 1006,
                    reason: format!("invalid URL: {e}"),
                    was_clean: false,
                },
            );
            inner
                .state
                .store(ReadyState::Closed.as_u8(), Ordering::SeqCst);
            close_command_channel(&inner);
            return;
        }
    };

    let (mut socket, _response) = match tungstenite::connect(request) {
        Ok(pair) => pair,
        Err(e) => {
            push_event(
                &inner,
                WebSocketEvent::Error(format!("connect failed: {e}")),
            );
            push_event(
                &inner,
                WebSocketEvent::Close {
                    code: 1006,
                    reason: e.to_string(),
                    was_clean: false,
                },
            );
            inner
                .state
                .store(ReadyState::Closed.as_u8(), Ordering::SeqCst);
            close_command_channel(&inner);
            return;
        }
    };

    inner
        .state
        .store(ReadyState::Open.as_u8(), Ordering::SeqCst);
    push_event(&inner, WebSocketEvent::Open);

    // Set the underlying TCP stream non-blocking so the worker can interleave
    // outbound commands with inbound frames without dedicated reader threads.
    if let Some(tcp) = inner_tcp(socket.get_ref()) {
        let _ = tcp.set_nonblocking(true);
    }

    let mut close_code = 1006u16;
    let mut close_reason = String::new();
    let mut was_clean = false;

    loop {
        // Drain any outbound commands without blocking.
        loop {
            match cmd_rx.try_recv() {
                Ok(OutboundCommand::Send(msg)) => {
                    let bytes = message_byte_len(&msg);
                    match socket.send(msg) {
                        Ok(_) => {
                            inner.buffered.fetch_sub(bytes, Ordering::SeqCst);
                        }
                        Err(e) => {
                            push_event(&inner, WebSocketEvent::Error(format!("send failed: {e}")));
                            close_code = 1006;
                            close_reason = e.to_string();
                            was_clean = false;
                            break;
                        }
                    }
                }
                Ok(OutboundCommand::Close { code, reason }) => {
                    let frame = tungstenite::protocol::frame::CloseFrame {
                        code: tungstenite::protocol::frame::coding::CloseCode::from(code),
                        reason: reason.clone().into(),
                    };
                    let _ = socket.close(Some(frame));
                    close_code = code;
                    close_reason = reason;
                    was_clean = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Application dropped the handle — initiate a clean close.
                    let _ = socket.close(None);
                    close_code = 1000;
                    close_reason = "handle dropped".into();
                    was_clean = true;
                    break;
                }
            }
        }

        // Try to read one inbound frame.
        match socket.read() {
            Ok(Message::Text(t)) => push_event(&inner, WebSocketEvent::Text(t.to_string())),
            Ok(Message::Binary(b)) => push_event(&inner, WebSocketEvent::Binary(b.to_vec())),
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(frame)) => {
                if let Some(f) = frame {
                    close_code = f.code.into();
                    close_reason = f.reason.to_string();
                    was_clean = true;
                }
                break;
            }
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                was_clean = true;
                break;
            }
            Err(tungstenite::Error::Io(ref io)) if io.kind() == std::io::ErrorKind::WouldBlock => {
                // No data right now — yield briefly to avoid busy-looping.
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                push_event(&inner, WebSocketEvent::Error(format!("read failed: {e}")));
                close_code = 1006;
                close_reason = e.to_string();
                was_clean = false;
                break;
            }
        }
    }

    inner
        .state
        .store(ReadyState::Closed.as_u8(), Ordering::SeqCst);
    push_event(
        &inner,
        WebSocketEvent::Close {
            code: close_code,
            reason: close_reason,
            was_clean,
        },
    );
    close_command_channel(&inner);
}

fn close_command_channel(inner: &Arc<WebSocketInner>) {
    if let Ok(mut guard) = inner.cmd_tx.lock() {
        *guard = None;
    }
}

fn message_byte_len(msg: &Message) -> u32 {
    match msg {
        Message::Text(t) => t.len() as u32,
        Message::Binary(b) => b.len() as u32,
        Message::Ping(b) | Message::Pong(b) => b.len() as u32,
        Message::Close(_) | Message::Frame(_) => 0,
    }
}

fn inner_tcp(stream: &MaybeTlsStream<TcpStream>) -> Option<&TcpStream> {
    match stream {
        MaybeTlsStream::Plain(s) => Some(s),
        _ => None,
    }
}

fn push_event(inner: &Arc<WebSocketInner>, event: WebSocketEvent) {
    if let Ok(mut q) = inner.events.lock() {
        q.push_back(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    use tungstenite::accept;

    fn drain_until<F>(ws: &WebSocket, predicate: F) -> Vec<WebSocketEvent>
    where
        F: Fn(&WebSocketEvent) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut collected = Vec::new();
        while Instant::now() < deadline {
            for ev in ws.poll_events() {
                let stop = predicate(&ev);
                collected.push(ev);
                if stop {
                    return collected;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        collected
    }

    #[test]
    fn echo_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            // Echo a single text message.
            if let Ok(Message::Text(t)) = socket.read() {
                socket.send(Message::Text(t)).unwrap();
            }
            let _ = socket.close(None);
        });

        let ws = WebSocket::connect(format!("ws://127.0.0.1:{port}"));
        let _ = drain_until(&ws, |e| matches!(e, WebSocketEvent::Open));
        ws.send_text("ping").unwrap();
        let events = drain_until(&ws, |e| matches!(e, WebSocketEvent::Text(_)));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WebSocketEvent::Text(t) if t == "ping"))
        );
        let _ = ws.close(1000, "bye");
        let _ = drain_until(&ws, |e| matches!(e, WebSocketEvent::Close { .. }));
        let _ = server.join();
    }

    #[test]
    fn ready_state_transitions() {
        let ws = WebSocket::connect("ws://127.0.0.1:1");
        // Either Connecting initially, or already Closed if connect failed quickly.
        let s0 = ws.ready_state();
        assert!(matches!(s0, ReadyState::Connecting | ReadyState::Closed));
        // Eventually transitions to Closed because the port is unreachable.
        let _ = drain_until(&ws, |e| matches!(e, WebSocketEvent::Close { .. }));
        assert_eq!(ws.ready_state(), ReadyState::Closed);
    }

    #[test]
    fn invalid_url_emits_error() {
        let ws = WebSocket::connect("not a url");
        let events = drain_until(&ws, |e| matches!(e, WebSocketEvent::Close { .. }));
        assert!(events.iter().any(|e| matches!(e, WebSocketEvent::Error(_))));
    }
}
