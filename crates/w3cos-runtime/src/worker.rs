//! Web Workers — W3C-standard background execution.
//!
//! This module mirrors the browser `Worker` and `SharedWorker` APIs on top of
//! native OS threads. Because W3C OS compiles applications to Rust ahead of
//! time, a "worker" is a Rust closure that runs on a dedicated thread and
//! exchanges JSON messages with its parent through MPSC channels — exactly the
//! semantics promised by the W3C HTML Living Standard ([web workers], [shared
//! workers]).
//!
//! ## Dedicated `Worker`
//!
//! ```ignore
//! use serde_json::json;
//! use w3cos_runtime::worker::{Worker, WorkerOptions};
//!
//! let worker = Worker::spawn(WorkerOptions::default(), |scope| {
//!     while let Some(msg) = scope.recv() {
//!         let n = msg.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
//!         scope.post_message(json!({"square": n * n})).ok();
//!     }
//! });
//!
//! worker.post_message(json!({"n": 7})).unwrap();
//! while let Some(reply) = worker.try_recv() {
//!     assert_eq!(reply["square"], json!(49));
//! }
//! worker.terminate();
//! ```
//!
//! ## Shared workers
//!
//! [`SharedWorker`] keeps a single worker thread alive across multiple
//! [`SharedWorker::port`]s, mirroring the W3C `SharedWorker` /
//! `MessagePort` pair. Each port is an independent send/receive endpoint
//! multiplexed through the worker's [`SharedWorkerScope`].
//!
//! ## Errors
//!
//! Following the spec, the `error` event is delivered as an [`WorkerEvent::Error`]
//! with a string message. [`Worker::poll_events`] returns the full ordered
//! event stream (including `message` and `error`) for reactive frame loops.
//!
//! [web workers]: https://html.spec.whatwg.org/multipage/workers.html
//! [shared workers]: https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworker-interface

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

/// Polling interval used by blocking `recv` calls to observe termination flags.
const RECV_POLL: Duration = Duration::from_millis(50);

/// `WorkerType` from the HTML spec — either a classic script or a module.
/// Recorded for compatibility but does not change runtime behavior because
/// the worker body is Rust code in W3C OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerType {
    Classic,
    Module,
}

impl Default for WorkerType {
    fn default() -> Self {
        WorkerType::Classic
    }
}

/// Mirrors `WorkerOptions` from the HTML spec (`name`, `type`, `credentials`).
#[derive(Debug, Clone, Default)]
pub struct WorkerOptions {
    pub name: Option<String>,
    pub worker_type: WorkerType,
}

impl WorkerOptions {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            worker_type: WorkerType::Classic,
        }
    }
}

/// One ordered event produced by a worker. The browser dispatches these as
/// `MessageEvent` / `ErrorEvent` on the parent scope; this enum lets reactive
/// frame loops drain them with [`Worker::poll_events`].
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Message(Value),
    Error(String),
    Exit,
}

#[derive(Default)]
struct EventBuffer {
    events: Vec<WorkerEvent>,
}

impl EventBuffer {
    fn push(&mut self, event: WorkerEvent) {
        self.events.push(event);
    }

    fn drain(&mut self) -> Vec<WorkerEvent> {
        std::mem::take(&mut self.events)
    }
}

/// A dedicated W3C Worker — owns one OS thread and one bidirectional channel.
///
/// Drop semantics match the spec's `Worker.terminate()`: dropping the handle
/// signals the worker to stop and joins it. Detach with
/// [`Worker::into_join_handle`] if you want the thread to outlive the handle.
pub struct Worker {
    name: Option<String>,
    out_tx: Option<mpsc::Sender<Value>>,
    events: Arc<Mutex<EventBuffer>>,
    terminate: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Worker {
    /// Spawn a dedicated worker. The closure runs on a fresh thread and
    /// receives a [`WorkerScope`] to communicate with the parent.
    pub fn spawn<F>(options: WorkerOptions, body: F) -> Self
    where
        F: FnOnce(&WorkerScope) + Send + 'static,
    {
        let (parent_to_worker_tx, parent_to_worker_rx) = mpsc::channel::<Value>();
        let (worker_to_parent_tx, worker_to_parent_rx) = mpsc::channel::<WorkerEvent>();
        let terminate = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(EventBuffer::default()));

        // Pump worker_to_parent_rx into the parent buffer. We use a small
        // "router" thread instead of exposing the rx so the parent can drain
        // events from any thread without locking ordering invariants.
        let router_buffer = events.clone();
        thread::Builder::new()
            .name("w3cos-worker-router".into())
            .spawn(move || {
                while let Ok(evt) = worker_to_parent_rx.recv() {
                    let is_exit = matches!(evt, WorkerEvent::Exit);
                    if let Ok(mut buf) = router_buffer.lock() {
                        buf.push(evt);
                    }
                    if is_exit {
                        break;
                    }
                }
            })
            .expect("spawn worker router");

        let scope = WorkerScope {
            inbox: Mutex::new(parent_to_worker_rx),
            outbox: worker_to_parent_tx.clone(),
            terminate: terminate.clone(),
        };

        let thread_name = options
            .name
            .clone()
            .unwrap_or_else(|| "w3cos-worker".to_string());
        let join = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                body(&scope);
                let _ = worker_to_parent_tx.send(WorkerEvent::Exit);
            })
            .expect("spawn dedicated worker");

        Self {
            name: options.name,
            out_tx: Some(parent_to_worker_tx),
            events,
            terminate,
            join: Some(join),
        }
    }

    /// Spec name attribute.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// W3C `Worker.postMessage(data)` — non-blocking.
    pub fn post_message(&self, data: Value) -> Result<(), WorkerError> {
        let tx = self.out_tx.as_ref().ok_or(WorkerError::Disconnected)?;
        tx.send(data).map_err(|_| WorkerError::Disconnected)
    }

    /// Returns the next pending message (drops error/exit events). Use
    /// [`Worker::poll_events`] when you need full fidelity.
    pub fn try_recv(&self) -> Option<Value> {
        let mut buf = self.events.lock().ok()?;
        let mut keep: Vec<WorkerEvent> = Vec::new();
        let mut found: Option<Value> = None;
        for evt in buf.drain() {
            match evt {
                WorkerEvent::Message(v) if found.is_none() => {
                    found = Some(v);
                }
                other => keep.push(other),
            }
        }
        buf.events = keep;
        found
    }

    /// Drain every pending event in arrival order.
    pub fn poll_events(&self) -> Vec<WorkerEvent> {
        match self.events.lock() {
            Ok(mut buf) => buf.drain(),
            Err(_) => Vec::new(),
        }
    }

    /// W3C `Worker.terminate()` — signal the worker to stop and join.
    /// The worker observes the flag through `WorkerScope::is_terminated` and
    /// through the inbound channel disconnect (which unblocks `recv`).
    pub fn terminate(mut self) {
        self.signal_terminate();
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }

    /// Drop the join handle without waiting; the OS thread keeps running until
    /// it returns naturally. Mirrors the spec's "garbage-collected worker that
    /// has live channels" behavior.
    pub fn into_join_handle(mut self) -> Option<JoinHandle<()>> {
        self.signal_terminate();
        self.join.take()
    }

    fn signal_terminate(&mut self) {
        self.terminate.store(true, Ordering::SeqCst);
        // Drop the only outbound sender so the worker scope's `recv` returns
        // `None` immediately — this is the cooperative termination signal.
        self.out_tx.take();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.signal_terminate();
        if let Some(handle) = self.join.take() {
            // Allow the worker thread to observe the dropped channel and exit.
            // We don't unwrap because a panicking worker should not poison the
            // host process.
            let _ = handle.join();
        }
    }
}

/// Worker-side scope — equivalent to `DedicatedWorkerGlobalScope` in the spec.
pub struct WorkerScope {
    inbox: Mutex<mpsc::Receiver<Value>>,
    outbox: mpsc::Sender<WorkerEvent>,
    terminate: Arc<AtomicBool>,
}

impl WorkerScope {
    /// `self.postMessage(data)` from inside the worker.
    pub fn post_message(&self, data: Value) -> Result<(), WorkerError> {
        self.outbox
            .send(WorkerEvent::Message(data))
            .map_err(|_| WorkerError::Disconnected)
    }

    /// Emit a worker-side error — surfaced as [`WorkerEvent::Error`] on the
    /// parent. Equivalent to dispatching an `ErrorEvent` in the browser.
    pub fn report_error(&self, message: impl Into<String>) -> Result<(), WorkerError> {
        self.outbox
            .send(WorkerEvent::Error(message.into()))
            .map_err(|_| WorkerError::Disconnected)
    }

    /// Blocking receive — returns `None` once the parent disconnects or
    /// terminates the worker.
    pub fn recv(&self) -> Option<Value> {
        let guard = self.inbox.lock().ok()?;
        loop {
            if self.is_terminated() {
                return None;
            }
            match guard.recv_timeout(RECV_POLL) {
                Ok(v) => return Some(v),
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }
    }

    /// Non-blocking receive — `None` when nothing is queued or after termination.
    pub fn try_recv(&self) -> Option<Value> {
        if self.is_terminated() {
            return None;
        }
        let guard = self.inbox.lock().ok()?;
        guard.try_recv().ok()
    }

    /// True after the parent calls [`Worker::terminate`].
    pub fn is_terminated(&self) -> bool {
        self.terminate.load(Ordering::SeqCst)
    }
}

/// Errors mirroring the union of `DOMException` cases the worker APIs raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerError {
    /// Channel closed: the peer has dropped or terminated.
    Disconnected,
    /// Provided port id does not exist on the shared worker.
    InvalidPort,
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::Disconnected => write!(f, "worker channel disconnected"),
            WorkerError::InvalidPort => write!(f, "invalid shared worker port"),
        }
    }
}

impl std::error::Error for WorkerError {}

// ---------------------------------------------------------------------------
// SharedWorker
// ---------------------------------------------------------------------------

/// Identifier for a [`SharedWorker`] port — equivalent to a `MessagePort`.
pub type PortId = u64;

/// A `SharedWorker` — one worker thread serves N parent ports.
///
/// Each [`SharedWorker::port`] returns a fresh [`SharedWorkerPort`] which
/// behaves like a dedicated `Worker` for the caller, but the underlying
/// thread is shared. This matches the W3C `SharedWorker.port` semantics and
/// lets multiple windows or modules talk to the same background instance.
pub struct SharedWorker {
    inner: Arc<SharedWorkerInner>,
    join: Option<JoinHandle<()>>,
}

struct SharedWorkerInner {
    name: Option<String>,
    next_port: AtomicU64,
    ports: Arc<Mutex<HashMap<PortId, mpsc::Sender<SharedWorkerEvent>>>>,
    inbox_tx: mpsc::Sender<(PortId, Value)>,
    terminate: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
enum SharedWorkerEvent {
    Message(Value),
    Error(String),
    Disconnect,
}

impl SharedWorker {
    /// Spawn a shared worker. The body receives a [`SharedWorkerScope`] that
    /// can address individual ports or broadcast.
    pub fn spawn<F>(options: WorkerOptions, body: F) -> Self
    where
        F: FnOnce(&SharedWorkerScope) + Send + 'static,
    {
        let (inbox_tx, inbox_rx) = mpsc::channel::<(PortId, Value)>();
        let terminate = Arc::new(AtomicBool::new(false));

        let inner = Arc::new(SharedWorkerInner {
            name: options.name.clone(),
            next_port: AtomicU64::new(1),
            ports: Arc::new(Mutex::new(HashMap::new())),
            inbox_tx,
            terminate: terminate.clone(),
        });

        let scope = SharedWorkerScope {
            inbox_rx: Mutex::new(inbox_rx),
            inner: inner.clone(),
        };

        let thread_name = options
            .name
            .clone()
            .unwrap_or_else(|| "w3cos-shared-worker".to_string());
        let join = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                body(&scope);
                let ports = scope.inner.ports.lock();
                if let Ok(map) = ports {
                    for tx in map.values() {
                        let _ = tx.send(SharedWorkerEvent::Disconnect);
                    }
                }
            })
            .expect("spawn shared worker");

        Self {
            inner,
            join: Some(join),
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    /// Mint a new port — equivalent to `new SharedWorker(...).port` in JS.
    pub fn port(&self) -> SharedWorkerPort {
        let (port_tx, port_rx) = mpsc::channel::<SharedWorkerEvent>();
        let id = self.inner.next_port.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut map) = self.inner.ports.lock() {
            map.insert(id, port_tx);
        }
        SharedWorkerPort {
            id,
            inbox: Arc::new(Mutex::new(port_rx)),
            inner: self.inner.clone(),
        }
    }

    /// Terminate the worker — disconnects every port, joins the thread.
    pub fn terminate(mut self) {
        self.inner.terminate.store(true, Ordering::SeqCst);
        if let Ok(mut map) = self.inner.ports.lock() {
            for tx in map.values() {
                let _ = tx.send(SharedWorkerEvent::Disconnect);
            }
            map.clear();
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for SharedWorker {
    fn drop(&mut self) {
        self.inner.terminate.store(true, Ordering::SeqCst);
        if let Ok(mut map) = self.inner.ports.lock() {
            for tx in map.values() {
                let _ = tx.send(SharedWorkerEvent::Disconnect);
            }
            map.clear();
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// One end of a [`SharedWorker`] connection.
pub struct SharedWorkerPort {
    id: PortId,
    inbox: Arc<Mutex<mpsc::Receiver<SharedWorkerEvent>>>,
    inner: Arc<SharedWorkerInner>,
}

impl SharedWorkerPort {
    pub fn id(&self) -> PortId {
        self.id
    }

    /// `port.postMessage(data)`.
    pub fn post_message(&self, data: Value) -> Result<(), WorkerError> {
        if self.inner.terminate.load(Ordering::SeqCst) {
            return Err(WorkerError::Disconnected);
        }
        self.inner
            .inbox_tx
            .send((self.id, data))
            .map_err(|_| WorkerError::Disconnected)
    }

    /// Try to receive a message — drops error/disconnect events.
    pub fn try_recv(&self) -> Option<Value> {
        let guard = self.inbox.lock().ok()?;
        loop {
            match guard.try_recv().ok()? {
                SharedWorkerEvent::Message(v) => return Some(v),
                _ => continue,
            }
        }
    }

    /// Drain every pending event (messages, errors, disconnect) in order.
    pub fn poll_events(&self) -> Vec<WorkerEvent> {
        let guard = match self.inbox.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        while let Ok(evt) = guard.try_recv() {
            match evt {
                SharedWorkerEvent::Message(v) => out.push(WorkerEvent::Message(v)),
                SharedWorkerEvent::Error(e) => out.push(WorkerEvent::Error(e)),
                SharedWorkerEvent::Disconnect => out.push(WorkerEvent::Exit),
            }
        }
        out
    }

    /// Detach this port — the shared worker will receive a disconnect event
    /// for this port id on its next poll.
    pub fn close(self) {
        if let Ok(mut map) = self.inner.ports.lock() {
            map.remove(&self.id);
        }
    }
}

impl Drop for SharedWorkerPort {
    fn drop(&mut self) {
        if let Ok(mut map) = self.inner.ports.lock() {
            map.remove(&self.id);
        }
    }
}

/// Worker-side scope of a [`SharedWorker`] — equivalent to
/// `SharedWorkerGlobalScope`.
pub struct SharedWorkerScope {
    inbox_rx: Mutex<mpsc::Receiver<(PortId, Value)>>,
    inner: Arc<SharedWorkerInner>,
}

impl SharedWorkerScope {
    /// Blocking receive — returns `(port_id, payload)` or `None` after termination.
    pub fn recv(&self) -> Option<(PortId, Value)> {
        let guard = self.inbox_rx.lock().ok()?;
        loop {
            if self.is_terminated() {
                return None;
            }
            match guard.recv_timeout(RECV_POLL) {
                Ok(v) => return Some(v),
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }
    }

    /// Non-blocking receive.
    pub fn try_recv(&self) -> Option<(PortId, Value)> {
        if self.is_terminated() {
            return None;
        }
        let guard = self.inbox_rx.lock().ok()?;
        guard.try_recv().ok()
    }

    /// Send a message to one specific port.
    pub fn send_to(&self, port: PortId, data: Value) -> Result<(), WorkerError> {
        let map = self
            .inner
            .ports
            .lock()
            .map_err(|_| WorkerError::Disconnected)?;
        let tx = map.get(&port).ok_or(WorkerError::InvalidPort)?;
        tx.send(SharedWorkerEvent::Message(data))
            .map_err(|_| WorkerError::Disconnected)
    }

    /// Broadcast — returns the number of ports that accepted the message.
    pub fn broadcast(&self, data: Value) -> usize {
        let map = match self.inner.ports.lock() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        let mut count = 0;
        for tx in map.values() {
            if tx
                .send(SharedWorkerEvent::Message(data.clone()))
                .is_ok()
            {
                count += 1;
            }
        }
        count
    }

    /// Report an error to a specific port.
    pub fn report_error_to(
        &self,
        port: PortId,
        message: impl Into<String>,
    ) -> Result<(), WorkerError> {
        let map = self
            .inner
            .ports
            .lock()
            .map_err(|_| WorkerError::Disconnected)?;
        let tx = map.get(&port).ok_or(WorkerError::InvalidPort)?;
        tx.send(SharedWorkerEvent::Error(message.into()))
            .map_err(|_| WorkerError::Disconnected)
    }

    /// Number of currently connected ports.
    pub fn port_count(&self) -> usize {
        self.inner.ports.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    pub fn is_terminated(&self) -> bool {
        self.inner.terminate.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn wait_for<T, F: FnMut() -> Option<T>>(mut f: F) -> Option<T> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(v) = f() {
                return Some(v);
            }
            thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn dedicated_worker_round_trip() {
        let worker = Worker::spawn(WorkerOptions::default(), |scope| {
            while let Some(msg) = scope.recv() {
                let n = msg.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
                scope
                    .post_message(json!({"square": n * n}))
                    .expect("send square");
            }
        });

        worker.post_message(json!({"n": 6})).unwrap();
        let reply = wait_for(|| worker.try_recv()).expect("worker reply");
        assert_eq!(reply["square"], json!(36));

        worker.post_message(json!({"n": 9})).unwrap();
        let reply = wait_for(|| worker.try_recv()).expect("second reply");
        assert_eq!(reply["square"], json!(81));

        worker.terminate();
    }

    #[test]
    fn worker_reports_error_event() {
        let worker = Worker::spawn(WorkerOptions::named("err-worker"), |scope| {
            let _ = scope.recv();
            scope.report_error("boom").unwrap();
        });

        worker.post_message(json!({})).unwrap();
        let events = wait_for(|| {
            let pending = worker.poll_events();
            if pending.iter().any(|e| matches!(e, WorkerEvent::Error(_))) {
                Some(pending)
            } else {
                None
            }
        })
        .expect("error event");

        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerEvent::Error(msg) if msg == "boom")));
    }

    #[test]
    fn worker_terminate_signals_scope() {
        let worker = Worker::spawn(WorkerOptions::default(), |scope| {
            // Block until termination is signalled; recv() must return None.
            assert!(scope.recv().is_none());
        });

        // Sleep briefly so the worker actually parks on `recv`.
        thread::sleep(Duration::from_millis(20));
        worker.terminate();
    }

    #[test]
    fn shared_worker_two_ports() {
        let shared = SharedWorker::spawn(WorkerOptions::named("sum-bus"), |scope| {
            while let Some((from, msg)) = scope.recv() {
                let n = msg.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
                scope
                    .send_to(from, json!({"echo": n + 1}))
                    .expect("send echo");
            }
        });

        let port_a = shared.port();
        let port_b = shared.port();
        assert_ne!(port_a.id(), port_b.id());

        port_a.post_message(json!({"n": 10})).unwrap();
        port_b.post_message(json!({"n": 20})).unwrap();

        let reply_a = wait_for(|| port_a.try_recv()).expect("port a reply");
        let reply_b = wait_for(|| port_b.try_recv()).expect("port b reply");
        assert_eq!(reply_a["echo"], json!(11));
        assert_eq!(reply_b["echo"], json!(21));

        shared.terminate();
    }

    #[test]
    fn shared_worker_broadcast() {
        let shared = SharedWorker::spawn(WorkerOptions::default(), |scope| {
            // Wait until two ports have connected, then broadcast once.
            for _ in 0..200 {
                if scope.port_count() >= 2 {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            scope.broadcast(json!({"hello": "everyone"}));
        });

        let port_a = shared.port();
        let port_b = shared.port();

        let recv_a = wait_for(|| port_a.try_recv()).expect("port a recv");
        let recv_b = wait_for(|| port_b.try_recv()).expect("port b recv");
        assert_eq!(recv_a["hello"], json!("everyone"));
        assert_eq!(recv_b["hello"], json!("everyone"));

        shared.terminate();
    }
}
