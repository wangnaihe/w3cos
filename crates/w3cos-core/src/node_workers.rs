//! `worker_threads` (and Electron's `utilityProcess`) host half (M3).
//!
//! A worker is an OS thread with its own JS realm: every `Value` graph,
//! module cache and timer queue in w3cos is thread-local, so the worker
//! re-runs the compiled program entry ([`register_entry`]) on a fresh thread
//! and the entry's boot prelude dispatches to the worker module (the
//! compiler links every statically named worker file into the entry). Messages
//! cross threads as structured-clone strings produced by the JS glue
//! (`node:internal/w3cos_clone`), so no `Rc` ever leaves its thread.
//!
//! Starting a worker asks the permission policy for scope `worker`.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::Value;
use crate::node_host::{arg, define};

/// The compiled program entry (`run_entry`), registered by generated code.
static ENTRY: OnceLock<fn() -> Value> = OnceLock::new();

/// Event-loop turns of the host runtime (microtasks, timers), registered by
/// the runtime that owns the timer queue.
#[derive(Clone, Copy)]
pub struct EventLoopHooks {
    pub drain_microtasks: fn() -> usize,
    pub tick_timers: fn() -> usize,
    pub next_timer_deadline: fn() -> Option<Instant>,
}

static EVENT_LOOP: OnceLock<EventLoopHooks> = OnceLock::new();

/// Called by generated code before the entry runs.
pub fn register_entry(entry: fn() -> Value) {
    let _ = ENTRY.set(entry);
}

/// ES-module worker entries linked into the program (absolute paths), see
/// [`run_worker_modules`].
static WORKER_MODULES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Called by generated code: these ES modules can be worker /
/// `utilityProcess` entries.
pub fn register_worker_modules(paths: &[&str]) {
    let mut modules = WORKER_MODULES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for path in paths {
        if !modules.iter().any(|known| known == path) {
            modules.push((*path).to_string());
        }
    }
}

/// Whether `path` is a registered ES-module worker entry.
pub fn is_worker_module(path: &str) -> bool {
    WORKER_MODULES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .any(|known| known == path)
}

/// The module this worker thread runs (`None` on the main thread).
pub fn worker_entry_path() -> Option<String> {
    CONTEXT.with(|slot| slot.borrow().as_ref().map(|context| context.path.clone()))
}

/// Run an ES-module worker entry on this worker thread: evaluate the
/// worker boot record first (`boot`, whose `entry()` sets up
/// `process.parentPort` for utility processes), then every module of the
/// entry's static closure in `order` (dependencies first), each after the
/// previous one's evaluation promise settles (top-level await).
pub fn run_worker_modules(boot: Option<&str>, order: &[&str]) -> Value {
    if let Some(boot) = boot {
        let _ = crate::module_registry::evaluate(boot);
        if let Some(exports) = crate::module_registry::export(boot, "default") {
            let mut exports = exports.read();
            // A CommonJS record's default export is its load-once thunk.
            if !exports.get_property("entry").is_callable() && exports.is_callable() {
                exports = exports.call(Value::Undefined, Vec::new());
            }
            let entry = exports.get_property("entry");
            if entry.is_callable() {
                entry.call(exports, Vec::new());
            }
        }
    }
    let mut evaluation = crate::promise::resolve(vec![Value::Undefined]);
    for specifier in order {
        let specifier = (*specifier).to_string();
        evaluation = crate::intrinsics::call_method(
            &evaluation,
            &Value::string("then"),
            vec![Value::function(move |_, _| {
                crate::module_registry::evaluate(&specifier).unwrap_or(Value::Undefined)
            })],
        );
    }
    evaluation
}

/// Called by the runtime that drives timers and microtasks.
pub fn register_event_loop(hooks: EventLoopHooks) {
    let _ = EVENT_LOOP.set(hooks);
}

static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(1);

/// Everything one worker shares with its parent.
struct Shared {
    to_child: Mutex<VecDeque<String>>,
    to_parent: Mutex<VecDeque<ParentEvent>>,
    stop: AtomicBool,
    exit_code: Mutex<Option<i32>>,
}

enum ParentEvent {
    Online,
    Message(String),
    /// Serialized uncaught error (`{name,message,stack}` JSON).
    Error(String),
    Exit(i32),
}

/// What the worker thread knows about itself.
struct Context {
    shared: Arc<Shared>,
    thread_id: u32,
    path: String,
    data: String,
    name: String,
    kind: String,
    env: String,
}

thread_local! {
    static CHILDREN: RefCell<HashMap<u32, Arc<Shared>>> = RefCell::new(HashMap::new());
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// Payload of the unwinding `process.exit()` inside a worker: not a JS
/// value, so `try`/`catch` in the worker cannot swallow it.
struct WorkerExit;

fn error_json(value: &Value) -> String {
    let field = |key: &str| {
        let v = value.get_property(key);
        if v.is_undefined() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(v.to_js_string())
        }
    };
    let is_object = value.is_object() || value.as_function().is_some();
    let json = if is_object && !value.get_property("message").is_undefined() {
        serde_json::json!({
            "name": field("name"),
            "message": field("message"),
            "stack": field("stack"),
            "code": field("code"),
        })
    } else {
        serde_json::json!({ "value": value.to_js_string() })
    };
    json.to_string()
}

fn run_worker(context: Context) {
    let shared = context.shared.clone();
    CONTEXT.with(|slot| *slot.borrow_mut() = Some(context));
    push_parent(&shared, ParentEvent::Online);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let entry = ENTRY.get().copied().expect("worker entry registered");
        entry();
        let Some(hooks) = EVENT_LOOP.get().copied() else {
            return;
        };
        loop {
            if shared.stop.load(Ordering::SeqCst) {
                break;
            }
            let ran = (hooks.drain_microtasks)() + (hooks.tick_timers)();
            if ran > 0 {
                continue;
            }
            match (hooks.next_timer_deadline)() {
                Some(at) => {
                    let wait = at.saturating_duration_since(Instant::now());
                    // Wake up regularly so `terminate()` is observed.
                    std::thread::sleep(wait.min(Duration::from_millis(5)));
                }
                None => break,
            }
        }
    }));
    let code = match outcome {
        Ok(()) => shared.exit_code.lock().unwrap().unwrap_or(0),
        Err(payload) => {
            if payload.is::<WorkerExit>() {
                shared.exit_code.lock().unwrap().unwrap_or(0)
            } else if let Some(value) = payload.downcast_ref::<crate::value::PanicValue>() {
                push_parent(&shared, ParentEvent::Error(error_json(&value.0)));
                1
            } else {
                let message = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "worker panicked".into());
                push_parent(
                    &shared,
                    ParentEvent::Error(
                        serde_json::json!({"name": "Error", "message": message}).to_string(),
                    ),
                );
                1
            }
        }
    };
    // A terminated worker exits with code 1, as in Node.
    let code = if shared.stop.load(Ordering::SeqCst) && shared.exit_code.lock().unwrap().is_none() {
        1
    } else {
        code
    };
    push_parent(&shared, ParentEvent::Exit(code));
    CONTEXT.with(|slot| slot.borrow_mut().take());
}

fn push_parent(shared: &Shared, event: ParentEvent) {
    shared.to_parent.lock().unwrap().push_back(event);
}

fn child(id: u32) -> Option<Arc<Shared>> {
    CHILDREN.with(|children| children.borrow().get(&id).cloned())
}

fn strings(values: Vec<String>) -> Value {
    Value::array(values.into_iter().map(Value::from).collect())
}

fn object(fields: Vec<(&str, Value)>) -> Value {
    Value::object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<HashMap<_, _>>(),
    )
}

/// Whether the current thread is a worker.
pub fn is_worker_thread() -> bool {
    CONTEXT.with(|slot| slot.borrow().is_some())
}

pub(crate) fn install(host: &Value) {
    // workerSpawn(path, data, name, kind, env) -> { id, threadId }
    define(host, "workerSpawn", |args| {
        let path = arg(&args, 0).to_js_string();
        crate::node_host_io::require_permission("worker", &path);
        if ENTRY.get().is_none() {
            let error = crate::builtins::error_instance(
                "Error",
                vec![Value::from(format!(
                    "Worker entry '{path}' was not compiled into this w3cos binary"
                ))],
            );
            error.set_property("code", Value::from("ERR_W3COS_UNSUPPORTED"));
            crate::throw_value(error);
        }
        let shared = Arc::new(Shared {
            to_child: Mutex::new(VecDeque::new()),
            to_parent: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            exit_code: Mutex::new(None),
        });
        let thread_id = NEXT_THREAD_ID.fetch_add(1, Ordering::SeqCst);
        let context = Context {
            shared: shared.clone(),
            thread_id,
            path: path.clone(),
            data: arg(&args, 1).to_js_string(),
            name: {
                let name = arg(&args, 2);
                if name.is_nullish() {
                    String::new()
                } else {
                    name.to_js_string()
                }
            },
            kind: {
                let kind = arg(&args, 3);
                if kind.is_nullish() {
                    "worker".into()
                } else {
                    kind.to_js_string()
                }
            },
            env: {
                let env = arg(&args, 4);
                if env.is_nullish() {
                    String::new()
                } else {
                    env.to_js_string()
                }
            },
        };
        let spawned = std::thread::Builder::new()
            .name(format!("w3cos-worker-{thread_id}"))
            .stack_size(64 << 20)
            .spawn(move || run_worker(context));
        if let Err(error) = spawned {
            crate::throw_value(crate::node_host::node_error(
                &error,
                "worker",
                Some(&path),
                None,
            ));
        }
        CHILDREN.with(|children| children.borrow_mut().insert(thread_id, shared));
        object(vec![
            ("id", Value::Number(thread_id as f64)),
            ("threadId", Value::Number(thread_id as f64)),
        ])
    });
    // workerPoll(id) -> { online, messages[], errors[], exited, code }
    define(host, "workerPoll", |args| {
        let id = arg(&args, 0).to_number() as u32;
        let Some(shared) = child(id) else {
            return Value::Undefined;
        };
        let events: Vec<ParentEvent> = shared.to_parent.lock().unwrap().drain(..).collect();
        let mut online = false;
        let mut messages = Vec::new();
        let mut errors = Vec::new();
        let mut exit = Value::Null;
        for event in events {
            match event {
                ParentEvent::Online => online = true,
                ParentEvent::Message(message) => messages.push(message),
                ParentEvent::Error(error) => errors.push(error),
                ParentEvent::Exit(code) => exit = Value::Number(code as f64),
            }
        }
        object(vec![
            ("online", Value::Bool(online)),
            ("messages", strings(messages)),
            ("errors", strings(errors)),
            ("exited", Value::Bool(!exit.is_null())),
            ("code", exit),
        ])
    });
    define(host, "workerPost", |args| {
        let id = arg(&args, 0).to_number() as u32;
        if let Some(shared) = child(id) {
            shared
                .to_child
                .lock()
                .unwrap()
                .push_back(arg(&args, 1).to_js_string());
        }
        Value::Undefined
    });
    define(host, "workerTerminate", |args| {
        let id = arg(&args, 0).to_number() as u32;
        if let Some(shared) = child(id) {
            shared.stop.store(true, Ordering::SeqCst);
        }
        Value::Undefined
    });
    define(host, "workerRelease", |args| {
        let id = arg(&args, 0).to_number() as u32;
        CHILDREN.with(|children| children.borrow_mut().remove(&id));
        Value::Undefined
    });
    // workerModuleCompiled(path) -> whether an ES-module worker entry is linked
    define(host, "workerModuleCompiled", |args| {
        Value::Bool(is_worker_module(&arg(&args, 0).to_js_string()))
    });
    // workerContext() -> null | { threadId, path, data, name, kind, env }
    define(host, "workerContext", |_| {
        CONTEXT.with(|slot| match slot.borrow().as_ref() {
            None => Value::Null,
            Some(context) => object(vec![
                ("threadId", Value::Number(context.thread_id as f64)),
                ("path", Value::from(context.path.as_str())),
                ("data", Value::from(context.data.as_str())),
                ("name", Value::from(context.name.as_str())),
                ("kind", Value::from(context.kind.as_str())),
                ("env", Value::from(context.env.as_str())),
            ]),
        })
    });
    define(host, "workerParentPost", |args| {
        CONTEXT.with(|slot| {
            if let Some(context) = slot.borrow().as_ref() {
                push_parent(
                    &context.shared,
                    ParentEvent::Message(arg(&args, 0).to_js_string()),
                );
            }
        });
        Value::Undefined
    });
    // workerParentPoll() -> messages[] (empty once the worker is stopping)
    define(host, "workerParentPoll", |_| {
        CONTEXT.with(|slot| match slot.borrow().as_ref() {
            None => Value::array(Vec::new()),
            Some(context) => {
                if context.shared.stop.load(Ordering::SeqCst) {
                    return Value::array(Vec::new());
                }
                strings(context.shared.to_child.lock().unwrap().drain(..).collect())
            }
        })
    });
    // workerExit(code): `process.exit()` inside a worker ends only the worker.
    define(host, "workerExit", |args| {
        let shared = CONTEXT.with(|slot| slot.borrow().as_ref().map(|c| c.shared.clone()));
        let Some(shared) = shared else {
            return Value::Undefined;
        };
        let code = arg(&args, 0).to_number();
        *shared.exit_code.lock().unwrap() = Some(if code.is_finite() { code as i32 } else { 0 });
        shared.stop.store(true, Ordering::SeqCst);
        std::panic::panic_any(WorkerExit)
    });
}
