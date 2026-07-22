//! JavaScript-visible IndexedDB surface.
//!
//! This module owns the Web API shape (`indexedDB`, requests and events). The
//! storage engine in `indexed_db` is deliberately hidden behind it and will be
//! replaced by SQLite without changing applications.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::rc::Weak;

use serde_json::Value as JsonValue;
use w3cos_core::JsObject;
use w3cos_core::Value;

use crate::indexed_db::{self, Database, IndexedDbError, KeyRange, Transaction, TransactionMode};
use crate::jsdom::queue_microtask_value;

type ListenerMap = Rc<RefCell<HashMap<String, Vec<Value>>>>;

struct RequestState {
    ready_state: &'static str,
    result_available: bool,
    result: Value,
    error: Value,
    source: Value,
    transaction: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WebTransactionState {
    Queued,
    Active,
    Finished,
    Aborted,
}

struct WebTransaction {
    id: u64,
    database_name: String,
    scope: Vec<String>,
    mode: TransactionMode,
    connection: Rc<DatabaseConnection>,
    backend: Transaction,
    state: Cell<WebTransactionState>,
    pending_requests: Cell<usize>,
    commit_requested: Cell<bool>,
    error: RefCell<Value>,
    listeners: ListenerMap,
    target: RefCell<Option<Weak<RefCell<JsObject>>>>,
}

struct DatabaseConnection {
    name: String,
    version: u32,
    closed: Cell<bool>,
    listeners: ListenerMap,
    target: RefCell<Option<Weak<RefCell<JsObject>>>>,
}

impl DatabaseConnection {
    fn target_value(&self) -> Option<Value> {
        self.target
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .map(Value::Object)
    }
}

struct OpenRequest {
    name: String,
    requested_version: Option<u32>,
    request: Value,
    state: Rc<RefCell<RequestState>>,
    listeners: ListenerMap,
    notified_connections: Cell<bool>,
    blocked_fired: Cell<bool>,
    waiting: Cell<bool>,
}

struct DeleteRequest {
    name: String,
    request: Value,
    state: Rc<RefCell<RequestState>>,
    listeners: ListenerMap,
    notified_connections: Cell<bool>,
    blocked_fired: Cell<bool>,
    waiting: Cell<bool>,
}

struct CursorState {
    transaction: Rc<WebTransaction>,
    store_name: String,
    entries: Vec<CursorEntry>,
    position: Cell<usize>,
    direction: &'static str,
    key_only: bool,
    request: Value,
    request_state: Rc<RefCell<RequestState>>,
    listeners: ListenerMap,
    advancing: Cell<bool>,
}

struct CursorEntry {
    key: JsonValue,
    primary_key: JsonValue,
    value: JsonValue,
}

#[derive(Default)]
struct ConnectionCoordinator {
    connections: HashMap<String, Vec<Weak<DatabaseConnection>>>,
    pending_opens: HashMap<String, Vec<Rc<OpenRequest>>>,
    pending_deletes: HashMap<String, Vec<Rc<DeleteRequest>>>,
}

impl WebTransaction {
    fn target_value(&self) -> Option<Value> {
        self.target
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .map(Value::Object)
    }
}

#[derive(Default)]
struct TransactionCoordinator {
    next_id: u64,
    active: Vec<Weak<WebTransaction>>,
    waiting: Vec<Weak<WebTransaction>>,
}

thread_local! {
    static TRANSACTION_COORDINATOR: RefCell<TransactionCoordinator> =
        RefCell::new(TransactionCoordinator::default());
    static CONNECTION_COORDINATOR: RefCell<ConnectionCoordinator> =
        RefCell::new(ConnectionCoordinator::default());
}

impl Default for RequestState {
    fn default() -> Self {
        Self {
            ready_state: "pending",
            result_available: false,
            result: Value::Undefined,
            error: Value::Null,
            source: Value::Null,
            transaction: Value::Null,
        }
    }
}

fn func(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    Value::function(f)
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn request_value() -> (Value, Rc<RefCell<RequestState>>, ListenerMap) {
    let state = Rc::new(RefCell::new(RequestState::default()));
    let listeners: ListenerMap = Rc::new(RefCell::new(HashMap::new()));
    let mut properties = HashMap::new();

    for event in ["success", "error", "upgradeneeded", "blocked"] {
        properties.insert(format!("on{event}"), Value::Null);
    }

    let ready_state = state.clone();
    properties.insert(
        "__w3cos_getter_readyState".into(),
        func(move |_, _| Value::from(ready_state.borrow().ready_state)),
    );
    let result = state.clone();
    properties.insert(
        "__w3cos_getter_result".into(),
        func(move |_, _| {
            if !result.borrow().result_available {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "The request has not finished.",
                ));
            }
            result.borrow().result.clone()
        }),
    );
    let error = state.clone();
    properties.insert(
        "__w3cos_getter_error".into(),
        func(move |_, _| {
            if error.borrow().ready_state == "pending" {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "The request has not finished.",
                ));
            }
            error.borrow().error.clone()
        }),
    );
    let source = state.clone();
    properties.insert(
        "__w3cos_getter_source".into(),
        func(move |_, _| source.borrow().source.clone()),
    );
    let transaction = state.clone();
    properties.insert(
        "__w3cos_getter_transaction".into(),
        func(move |_, _| transaction.borrow().transaction.clone()),
    );

    let add_listeners = listeners.clone();
    properties.insert(
        "addEventListener".into(),
        func(move |_, args| {
            let event_type = arg(&args, 0).to_js_string();
            let listener = arg(&args, 1);
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
    let remove_listeners = listeners.clone();
    properties.insert(
        "removeEventListener".into(),
        func(move |_, args| {
            let event_type = arg(&args, 0).to_js_string();
            let listener = arg(&args, 1);
            if let Some(entries) = remove_listeners.borrow_mut().get_mut(&event_type) {
                entries.retain(|candidate| !same_callback(candidate, &listener));
            }
            Value::Undefined
        }),
    );

    (Value::object(properties), state, listeners)
}

fn same_callback(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Function(left), Value::Function(right)) => left.ptr_eq(right),
        (Value::Object(left), Value::Object(right)) => Rc::ptr_eq(left, right),
        _ => left == right,
    }
}

fn transactions_conflict(left: &WebTransaction, right: &WebTransaction) -> bool {
    left.database_name == right.database_name
        && (left.mode == TransactionMode::ReadWrite || right.mode == TransactionMode::ReadWrite)
        && left
            .scope
            .iter()
            .any(|name| right.scope.iter().any(|other| other == name))
}

fn register_transaction(
    connection: Rc<DatabaseConnection>,
    scope: Vec<String>,
    mode: TransactionMode,
    backend: Transaction,
) -> Rc<WebTransaction> {
    TRANSACTION_COORDINATOR.with(|coordinator| {
        let mut coordinator = coordinator.borrow_mut();
        coordinator.active.retain(|transaction| {
            transaction
                .upgrade()
                .is_some_and(|transaction| transaction.state.get() == WebTransactionState::Active)
        });
        let id = coordinator.next_id;
        coordinator.next_id += 1;
        let transaction = Rc::new(WebTransaction {
            id,
            database_name: connection.name.clone(),
            scope,
            mode,
            connection,
            backend,
            state: Cell::new(WebTransactionState::Queued),
            pending_requests: Cell::new(0),
            commit_requested: Cell::new(false),
            error: RefCell::new(Value::Null),
            listeners: Rc::new(RefCell::new(HashMap::new())),
            target: RefCell::new(None),
        });
        let blocked = coordinator.active.iter().any(|active| {
            active
                .upgrade()
                .is_some_and(|active| transactions_conflict(&active, &transaction))
        }) || coordinator.waiting.iter().any(|waiting| {
            waiting
                .upgrade()
                .is_some_and(|waiting| transactions_conflict(&waiting, &transaction))
        });
        if blocked {
            coordinator.waiting.push(Rc::downgrade(&transaction));
        } else {
            transaction.state.set(WebTransactionState::Active);
            coordinator.active.push(Rc::downgrade(&transaction));
        }
        transaction
    })
}

fn release_transaction(transaction: &Rc<WebTransaction>) {
    TRANSACTION_COORDINATOR.with(|coordinator| {
        let mut coordinator = coordinator.borrow_mut();
        coordinator.active.retain(|active| {
            active
                .upgrade()
                .is_some_and(|active| active.id != transaction.id)
        });
        coordinator.waiting.retain(|waiting| {
            waiting.upgrade().is_some_and(|waiting| {
                waiting.id != transaction.id && waiting.state.get() == WebTransactionState::Queued
            })
        });
        loop {
            let Some(waiting) = coordinator.waiting.first().and_then(Weak::upgrade) else {
                if !coordinator.waiting.is_empty() {
                    coordinator.waiting.remove(0);
                    continue;
                }
                break;
            };
            if waiting.state.get() != WebTransactionState::Queued {
                coordinator.waiting.remove(0);
                continue;
            }
            let blocked = coordinator.active.iter().any(|active| {
                active
                    .upgrade()
                    .is_some_and(|active| transactions_conflict(&active, &waiting))
            });
            if blocked {
                break;
            }
            coordinator.waiting.remove(0);
            waiting.state.set(WebTransactionState::Active);
            coordinator.active.push(Rc::downgrade(&waiting));
        }
    });
}

fn event_value(event_type: &str, target: &Value, details: &[(&str, Value)]) -> Value {
    let cancelable = event_type == "error";
    let default_prevented = Rc::new(Cell::new(false));
    let mut properties = HashMap::from([
        ("type".into(), Value::from(event_type)),
        ("target".into(), target.clone()),
        ("currentTarget".into(), target.clone()),
        ("bubbles".into(), Value::Bool(event_type == "error")),
        ("cancelable".into(), Value::Bool(cancelable)),
    ]);
    let prevented = default_prevented.clone();
    properties.insert(
        "__w3cos_getter_defaultPrevented".into(),
        func(move |_, _| Value::Bool(prevented.get())),
    );
    properties.insert(
        "preventDefault".into(),
        func(move |_, _| {
            if cancelable {
                default_prevented.set(true);
            }
            Value::Undefined
        }),
    );
    for (key, value) in details {
        properties.insert((*key).into(), value.clone());
    }
    Value::object(properties)
}

fn fire_event(
    request: &Value,
    listeners: &ListenerMap,
    event_type: &str,
    details: &[(&str, Value)],
) -> bool {
    let event = event_value(event_type, request, details);
    let handler = request.get_property(&format!("on{event_type}"));
    if handler.is_function() {
        handler.call(request.clone(), vec![event.clone()]);
    }
    let callbacks = listeners
        .borrow()
        .get(event_type)
        .cloned()
        .unwrap_or_default();
    for callback in callbacks {
        callback.call(request.clone(), vec![event.clone()]);
    }
    event.get_property("defaultPrevented").to_bool()
}

fn complete_success(
    request: &Value,
    state: &Rc<RefCell<RequestState>>,
    listeners: &ListenerMap,
    result: Value,
) {
    {
        let mut state = state.borrow_mut();
        state.ready_state = "done";
        state.result_available = true;
        state.result = result;
        state.error = Value::Null;
    }
    let _ = fire_event(request, listeners, "success", &[]);
}

fn complete_error(
    request: &Value,
    state: &Rc<RefCell<RequestState>>,
    listeners: &ListenerMap,
    error: IndexedDbError,
) -> bool {
    {
        let mut state = state.borrow_mut();
        state.ready_state = "done";
        state.result_available = false;
        state.result = Value::Undefined;
        state.error = dom_exception(&error.name, &error.message);
    }
    fire_event(request, listeners, "error", &[])
}

fn fire_transaction_event(transaction: &WebTransaction, event_type: &str) {
    let Some(target) = transaction.target_value() else {
        return;
    };
    let _ = fire_event(&target, &transaction.listeners, event_type, &[]);
}

fn fire_transaction_error(transaction: &WebTransaction) -> bool {
    let Some(target) = transaction.target_value() else {
        return false;
    };
    fire_event(&target, &transaction.listeners, "error", &[])
}

fn fire_database_error(connection: &DatabaseConnection) -> bool {
    let Some(target) = connection.target_value() else {
        return false;
    };
    fire_event(&target, &connection.listeners, "error", &[])
}

fn finish_transaction(transaction: &Rc<WebTransaction>) {
    if transaction.state.get() != WebTransactionState::Active
        || transaction.pending_requests.get() != 0
    {
        return;
    }
    match transaction.backend.commit() {
        Ok(()) => {
            transaction.state.set(WebTransactionState::Finished);
            fire_transaction_event(transaction, "complete");
        }
        Err(_) => {
            let _ = transaction.backend.abort();
            *transaction.error.borrow_mut() =
                dom_exception("UnknownError", "The transaction could not be committed.");
            transaction.state.set(WebTransactionState::Aborted);
            fire_transaction_event(transaction, "error");
            fire_transaction_event(transaction, "abort");
        }
    }
    release_transaction(transaction);
}

fn abort_transaction(transaction: &Rc<WebTransaction>) {
    if matches!(
        transaction.state.get(),
        WebTransactionState::Finished | WebTransactionState::Aborted
    ) {
        w3cos_core::throw_value(dom_exception(
            "InvalidStateError",
            "The transaction has already finished.",
        ));
    }
    let _ = transaction.backend.abort();
    transaction.state.set(WebTransactionState::Aborted);
    fire_transaction_event(transaction, "abort");
    release_transaction(transaction);
}

fn queue_transaction_finalize(transaction: Rc<WebTransaction>) {
    queue_microtask_value(func(move |_, _| {
        match transaction.state.get() {
            WebTransactionState::Queued => queue_transaction_finalize(transaction.clone()),
            WebTransactionState::Active if transaction.pending_requests.get() == 0 => {
                finish_transaction(&transaction)
            }
            _ => {}
        }
        Value::Undefined
    }));
}

fn dom_exception(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::from(name)),
        ("message".into(), Value::from(message)),
        ("code".into(), Value::Number(0.0)),
    ]))
}

fn live_connections(name: &str) -> Vec<Rc<DatabaseConnection>> {
    CONNECTION_COORDINATOR.with(|coordinator| {
        let mut coordinator = coordinator.borrow_mut();
        let connections = coordinator.connections.entry(name.to_string()).or_default();
        connections.retain(|connection| {
            connection
                .upgrade()
                .is_some_and(|connection| !connection.closed.get())
        });
        connections.iter().filter_map(Weak::upgrade).collect()
    })
}

fn register_connection(connection: &Rc<DatabaseConnection>) {
    if connection.closed.get() {
        return;
    }
    CONNECTION_COORDINATOR.with(|coordinator| {
        coordinator
            .borrow_mut()
            .connections
            .entry(connection.name.clone())
            .or_default()
            .push(Rc::downgrade(connection));
    });
}

fn queue_open_attempt(open: Rc<OpenRequest>) {
    queue_microtask_value(func(move |_, _| {
        attempt_open(open.clone());
        Value::Undefined
    }));
}

fn resume_pending_opens(name: &str) {
    let (opens, deletes) = CONNECTION_COORDINATOR.with(|coordinator| {
        let mut coordinator = coordinator.borrow_mut();
        (
            coordinator.pending_opens.remove(name).unwrap_or_default(),
            coordinator.pending_deletes.remove(name).unwrap_or_default(),
        )
    });
    for pending in opens {
        pending.waiting.set(false);
        queue_open_attempt(pending);
    }
    for pending in deletes {
        pending.waiting.set(false);
        queue_delete_attempt(pending);
    }
}

fn close_connection(connection: &Rc<DatabaseConnection>) {
    if connection.closed.replace(true) {
        return;
    }
    CONNECTION_COORDINATOR.with(|coordinator| {
        if let Some(connections) = coordinator
            .borrow_mut()
            .connections
            .get_mut(&connection.name)
        {
            connections.retain(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|candidate| !candidate.closed.get())
            });
        }
    });
    resume_pending_opens(&connection.name);
}

fn attempt_open(open: Rc<OpenRequest>) {
    let version = match open.requested_version {
        Some(version) => version,
        None => match indexed_db::current_version(&open.name) {
            Ok(0) => 1,
            Ok(version) => version,
            Err(error) => {
                let _ = complete_error(&open.request, &open.state, &open.listeners, error);
                return;
            }
        },
    };
    let old_version = match indexed_db::current_version(&open.name) {
        Ok(version) => version,
        Err(error) => {
            let _ = complete_error(&open.request, &open.state, &open.listeners, error);
            return;
        }
    };

    if version > old_version {
        if !open.notified_connections.replace(true) {
            for connection in live_connections(&open.name) {
                let Some(target) = connection.target_value() else {
                    continue;
                };
                let _ = fire_event(
                    &target,
                    &connection.listeners,
                    "versionchange",
                    &[
                        ("oldVersion", Value::Number(connection.version as f64)),
                        ("newVersion", Value::Number(version as f64)),
                    ],
                );
            }
        }

        if !live_connections(&open.name).is_empty() {
            if !open.blocked_fired.replace(true) {
                let _ = fire_event(
                    &open.request,
                    &open.listeners,
                    "blocked",
                    &[
                        ("oldVersion", Value::Number(old_version as f64)),
                        ("newVersion", Value::Number(version as f64)),
                    ],
                );
            }
            if !open.waiting.replace(true) {
                CONNECTION_COORDINATOR.with(|coordinator| {
                    coordinator
                        .borrow_mut()
                        .pending_opens
                        .entry(open.name.clone())
                        .or_default()
                        .push(open.clone());
                });
            }
            return;
        }
    }

    let upgrading = Rc::new(Cell::new(false));
    let upgrade_aborted = Rc::new(Cell::new(false));
    let database_value = Rc::new(RefCell::new(None::<(Value, Rc<DatabaseConnection>)>));
    let result = indexed_db::open(&open.name, version, {
        let open = open.clone();
        let upgrading = upgrading.clone();
        let upgrade_aborted = upgrade_aborted.clone();
        let database_value = database_value.clone();
        move |database, old_version, new_version| {
            upgrading.set(true);
            let value = database_web_value(database.clone(), upgrading.clone(), new_version);
            *database_value.borrow_mut() = Some(value.clone());
            {
                let mut state = open.state.borrow_mut();
                state.result = value.0;
                state.result_available = true;
                state.transaction = versionchange_transaction_value(
                    database.clone(),
                    upgrading.clone(),
                    upgrade_aborted.clone(),
                );
            }
            let _ = fire_event(
                &open.request,
                &open.listeners,
                "upgradeneeded",
                &[
                    ("oldVersion", Value::Number(old_version as f64)),
                    ("newVersion", Value::Number(new_version as f64)),
                ],
            );
            if upgrade_aborted.get() {
                return Err(IndexedDbError {
                    name: "AbortError".into(),
                    message: "The versionchange transaction was aborted.".into(),
                });
            }
            upgrading.set(false);
            Ok(())
        }
    });
    match result {
        Ok(database) => {
            open.state.borrow_mut().transaction = Value::Null;
            let (value, connection) = database_value.borrow_mut().take().unwrap_or_else(|| {
                let visible_version = database.version();
                database_web_value(database, Rc::new(Cell::new(false)), visible_version)
            });
            register_connection(&connection);
            complete_success(&open.request, &open.state, &open.listeners, value);
        }
        Err(error) => {
            open.state.borrow_mut().transaction = Value::Null;
            let _ = complete_error(&open.request, &open.state, &open.listeners, error);
        }
    }
}

fn versionchange_transaction_value(
    database: Database,
    upgrading: Rc<Cell<bool>>,
    aborted: Rc<Cell<bool>>,
) -> Value {
    let names_database = database.clone();
    let object_store_database = database.clone();
    let object_store_upgrading = upgrading.clone();
    Value::object(HashMap::from([
        ("mode".into(), Value::from("versionchange")),
        ("durability".into(), Value::from("default")),
        ("error".into(), Value::Null),
        (
            "__w3cos_getter_objectStoreNames".into(),
            func(move |_, _| {
                Value::array(
                    names_database
                        .object_store_names()
                        .into_iter()
                        .map(Value::from)
                        .collect(),
                )
            }),
        ),
        (
            "objectStore".into(),
            func(move |_, args| {
                let name = arg(&args, 0).to_js_string();
                if !object_store_database.object_store_names().contains(&name) {
                    w3cos_core::throw_value(dom_exception(
                        "NotFoundError",
                        "The requested object store does not exist.",
                    ));
                }
                object_store_schema_value(
                    object_store_database.clone(),
                    name,
                    object_store_upgrading.clone(),
                )
            }),
        ),
        (
            "abort".into(),
            func(move |_, _| {
                aborted.set(true);
                Value::Undefined
            }),
        ),
    ]))
}

fn queue_delete_attempt(request: Rc<DeleteRequest>) {
    queue_microtask_value(func(move |_, _| {
        attempt_delete(request.clone());
        Value::Undefined
    }));
}

fn attempt_delete(request: Rc<DeleteRequest>) {
    let old_version = match indexed_db::current_version(&request.name) {
        Ok(version) => version,
        Err(error) => {
            let _ = complete_error(&request.request, &request.state, &request.listeners, error);
            return;
        }
    };
    if !request.notified_connections.replace(true) {
        for connection in live_connections(&request.name) {
            let Some(target) = connection.target_value() else {
                continue;
            };
            let _ = fire_event(
                &target,
                &connection.listeners,
                "versionchange",
                &[
                    ("oldVersion", Value::Number(connection.version as f64)),
                    ("newVersion", Value::Null),
                ],
            );
        }
    }
    if !live_connections(&request.name).is_empty() {
        if !request.blocked_fired.replace(true) {
            let _ = fire_event(
                &request.request,
                &request.listeners,
                "blocked",
                &[
                    ("oldVersion", Value::Number(old_version as f64)),
                    ("newVersion", Value::Null),
                ],
            );
        }
        if !request.waiting.replace(true) {
            CONNECTION_COORDINATOR.with(|coordinator| {
                coordinator
                    .borrow_mut()
                    .pending_deletes
                    .entry(request.name.clone())
                    .or_default()
                    .push(request.clone());
            });
        }
        return;
    }
    match indexed_db::delete(&request.name) {
        Ok(()) => complete_success(
            &request.request,
            &request.state,
            &request.listeners,
            Value::Undefined,
        ),
        Err(error) => {
            let _ = complete_error(&request.request, &request.state, &request.listeners, error);
        }
    }
}

/// Standard `IDBFactory` installed as `globalThis.indexedDB`.
pub fn factory_value() -> Value {
    let mut properties = HashMap::new();
    properties.insert(
        "open".into(),
        func(|_, args| {
            let name = arg(&args, 0).to_js_string();
            let version_value = arg(&args, 1);
            let requested_version = if version_value.is_undefined() {
                None
            } else {
                Some(version_value.to_u32())
            };
            if requested_version == Some(0) {
                w3cos_core::throw_value(dom_exception(
                    "TypeError",
                    "The database version must be greater than zero.",
                ));
            }

            let (request, state, listeners) = request_value();
            queue_open_attempt(Rc::new(OpenRequest {
                name,
                requested_version,
                request: request.clone(),
                state,
                listeners,
                notified_connections: Cell::new(false),
                blocked_fired: Cell::new(false),
                waiting: Cell::new(false),
            }));
            request
        }),
    );
    properties.insert(
        "deleteDatabase".into(),
        func(|_, args| {
            let name = arg(&args, 0).to_js_string();
            let (request, state, listeners) = request_value();
            queue_delete_attempt(Rc::new(DeleteRequest {
                name,
                request: request.clone(),
                state,
                listeners,
                notified_connections: Cell::new(false),
                blocked_fired: Cell::new(false),
                waiting: Cell::new(false),
            }));
            request
        }),
    );
    properties.insert(
        "cmp".into(),
        func(|_, args| {
            let left = match value_to_json(&arg(&args, 0)) {
                Ok(value) => value,
                Err(error) => w3cos_core::throw_value(dom_exception(&error.name, &error.message)),
            };
            let right = match value_to_json(&arg(&args, 1)) {
                Ok(value) => value,
                Err(error) => w3cos_core::throw_value(dom_exception(&error.name, &error.message)),
            };
            match indexed_db::compare_keys(&left, &right) {
                Ok(Ordering::Less) => Value::Number(-1.0),
                Ok(Ordering::Equal) => Value::Number(0.0),
                Ok(Ordering::Greater) => Value::Number(1.0),
                Err(error) => w3cos_core::throw_value(dom_exception(&error.name, &error.message)),
            }
        }),
    );
    properties.insert(
        "databases".into(),
        func(|_, _| {
            let databases = indexed_db::databases()
                .into_iter()
                .filter_map(|name| {
                    indexed_db::current_version(&name).ok().map(|version| {
                        Value::object(HashMap::from([
                            ("name".into(), Value::from(name)),
                            ("version".into(), Value::Number(version as f64)),
                        ]))
                    })
                })
                .collect();
            w3cos_core::promise::resolve(vec![Value::array(databases)])
        }),
    );
    Value::object(properties)
}

pub fn key_range_constructor_value() -> Value {
    let mut properties = HashMap::new();
    properties.insert(
        "only".into(),
        func(|_, args| {
            let key = arg(&args, 0);
            validate_key_range(value_to_json(&key).and_then(|key| KeyRange::only(&key)));
            key_range_value(key.clone(), key, false, false)
        }),
    );
    properties.insert(
        "lowerBound".into(),
        func(|_, args| {
            let lower = arg(&args, 0);
            let open = arg(&args, 1).to_bool();
            validate_key_range(
                value_to_json(&lower).and_then(|key| KeyRange::lower_bound(&key, open)),
            );
            key_range_value(lower, Value::Undefined, open, false)
        }),
    );
    properties.insert(
        "upperBound".into(),
        func(|_, args| {
            let upper = arg(&args, 0);
            let open = arg(&args, 1).to_bool();
            validate_key_range(
                value_to_json(&upper).and_then(|key| KeyRange::upper_bound(&key, open)),
            );
            key_range_value(Value::Undefined, upper, false, open)
        }),
    );
    properties.insert(
        "bound".into(),
        func(|_, args| {
            let lower = arg(&args, 0);
            let upper = arg(&args, 1);
            let lower_open = arg(&args, 2).to_bool();
            let upper_open = arg(&args, 3).to_bool();
            validate_key_range(value_to_json(&lower).and_then(|lower| {
                value_to_json(&upper)
                    .and_then(|upper| KeyRange::bound(&lower, &upper, lower_open, upper_open))
            }));
            key_range_value(lower, upper, lower_open, upper_open)
        }),
    );
    Value::object(properties)
}

fn validate_key_range(result: indexed_db::Result<KeyRange>) {
    if let Err(error) = result {
        w3cos_core::throw_value(dom_exception(&error.name, &error.message));
    }
}

fn key_range_value(lower: Value, upper: Value, lower_open: bool, upper_open: bool) -> Value {
    Value::object(HashMap::from([
        ("__w3cos_idb_key_range".into(), Value::Bool(true)),
        ("lower".into(), lower),
        ("upper".into(), upper),
        ("lowerOpen".into(), Value::Bool(lower_open)),
        ("upperOpen".into(), Value::Bool(upper_open)),
    ]))
}

fn key_range_from_query(query: &Value) -> indexed_db::Result<Option<KeyRange>> {
    if query.is_undefined() {
        return Ok(None);
    }
    if query.get_property("__w3cos_idb_key_range").to_bool() {
        let lower = query.get_property("lower");
        let upper = query.get_property("upper");
        let lower_open = query.get_property("lowerOpen").to_bool();
        let upper_open = query.get_property("upperOpen").to_bool();
        return match (lower.is_undefined(), upper.is_undefined()) {
            (false, false) => KeyRange::bound(
                &value_to_json(&lower)?,
                &value_to_json(&upper)?,
                lower_open,
                upper_open,
            )
            .map(Some),
            (false, true) => KeyRange::lower_bound(&value_to_json(&lower)?, lower_open).map(Some),
            (true, false) => KeyRange::upper_bound(&value_to_json(&upper)?, upper_open).map(Some),
            (true, true) => Err(IndexedDbError {
                name: "DataError".into(),
                message: "A key range must have at least one bound.".into(),
            }),
        };
    }
    KeyRange::only(&value_to_json(query)?).map(Some)
}

fn key_path_from_web(value: &Value, optional: bool) -> indexed_db::Result<String> {
    if value.is_undefined() || value.is_null() {
        if optional {
            return Ok(indexed_db::encode_key_path(None));
        }
        return Err(IndexedDbError {
            name: "SyntaxError".into(),
            message: "A key path is required.".into(),
        });
    }
    if let Value::String(path) = value {
        return Ok(indexed_db::encode_key_path(Some(std::slice::from_ref(
            path,
        ))));
    }
    if let Value::Array(paths) = value {
        let paths = paths
            .borrow()
            .iter()
            .map(|path| match path {
                Value::String(path) => Ok(path.clone()),
                _ => Err(IndexedDbError {
                    name: "SyntaxError".into(),
                    message: "Compound key paths must contain only strings.".into(),
                }),
            })
            .collect::<indexed_db::Result<Vec<_>>>()?;
        if paths.is_empty() {
            return Err(IndexedDbError {
                name: "SyntaxError".into(),
                message: "A compound key path cannot be empty.".into(),
            });
        }
        return Ok(indexed_db::encode_key_path(Some(&paths)));
    }
    Err(IndexedDbError {
        name: "SyntaxError".into(),
        message: "A key path must be a string or sequence of strings.".into(),
    })
}

fn key_path_to_web(path: &str) -> Value {
    match indexed_db::key_path_parts(path) {
        None => Value::Null,
        Some(paths) if paths.len() == 1 => Value::from(paths[0].clone()),
        Some(paths) => Value::array(paths.into_iter().map(Value::from).collect()),
    }
}

fn database_web_value(
    database: Database,
    upgrading: Rc<Cell<bool>>,
    visible_version: u32,
) -> (Value, Rc<DatabaseConnection>) {
    let connection = Rc::new(DatabaseConnection {
        name: database.name().to_string(),
        version: visible_version,
        closed: Cell::new(false),
        listeners: Rc::new(RefCell::new(HashMap::new())),
        target: RefCell::new(None),
    });
    let mut properties = HashMap::from([
        ("name".into(), Value::from(database.name())),
        ("onabort".into(), Value::Null),
        ("onclose".into(), Value::Null),
        ("onerror".into(), Value::Null),
        ("onversionchange".into(), Value::Null),
    ]);
    let add_listeners = connection.listeners.clone();
    properties.insert(
        "addEventListener".into(),
        func(move |_, args| {
            let event_type = arg(&args, 0).to_js_string();
            let listener = arg(&args, 1);
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
    let remove_listeners = connection.listeners.clone();
    properties.insert(
        "removeEventListener".into(),
        func(move |_, args| {
            let event_type = arg(&args, 0).to_js_string();
            let listener = arg(&args, 1);
            if let Some(entries) = remove_listeners.borrow_mut().get_mut(&event_type) {
                entries.retain(|candidate| !same_callback(candidate, &listener));
            }
            Value::Undefined
        }),
    );
    properties.insert(
        "__w3cos_getter_version".into(),
        func(move |_, _| Value::Number(visible_version as f64)),
    );
    let names_database = database.clone();
    properties.insert(
        "__w3cos_getter_objectStoreNames".into(),
        func(move |_, _| {
            Value::array(
                names_database
                    .object_store_names()
                    .into_iter()
                    .map(Value::from)
                    .collect(),
            )
        }),
    );

    let create_database = database.clone();
    let create_upgrading = upgrading.clone();
    properties.insert(
        "createObjectStore".into(),
        func(move |_, args| {
            if !create_upgrading.get() {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "Object stores can only be created during versionchange.",
                ));
            }
            let name = arg(&args, 0).to_js_string();
            let options = arg(&args, 1);
            let key_path = key_path_from_web(&options.get_property("keyPath"), true)
                .unwrap_or_else(|error| {
                    w3cos_core::throw_value(dom_exception(&error.name, &error.message))
                });
            let auto_increment = options.get_property("autoIncrement").to_bool();
            if let Err(error) = create_database.create_object_store(&name, key_path, auto_increment)
            {
                w3cos_core::throw_value(dom_exception(&error.name, &error.message));
            }
            object_store_schema_value(create_database.clone(), name, create_upgrading.clone())
        }),
    );

    let delete_database = database.clone();
    properties.insert(
        "deleteObjectStore".into(),
        func(move |_, args| {
            if !upgrading.get() {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "Object stores can only be deleted during versionchange.",
                ));
            }
            if let Err(error) = delete_database.delete_object_store(&arg(&args, 0).to_js_string()) {
                w3cos_core::throw_value(dom_exception(&error.name, &error.message));
            }
            Value::Undefined
        }),
    );

    let transaction_database = database.clone();
    let transaction_connection = connection.clone();
    properties.insert(
        "transaction".into(),
        func(move |_, args| {
            if transaction_connection.closed.get() {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "The database connection is closed.",
                ));
            }
            let names_value = arg(&args, 0);
            let names = match names_value {
                Value::Array(values) => values
                    .borrow()
                    .iter()
                    .map(Value::to_js_string)
                    .collect::<Vec<_>>(),
                value => vec![value.to_js_string()],
            };
            let mode_value = arg(&args, 1);
            let mode = match mode_value.to_js_string().as_str() {
                "readwrite" => TransactionMode::ReadWrite,
                "readonly" | "undefined" => TransactionMode::ReadOnly,
                _ => w3cos_core::throw_value(dom_exception(
                    "TypeError",
                    "Transaction mode must be readonly or readwrite.",
                )),
            };
            let durability = match arg(&args, 2)
                .get_property("durability")
                .to_js_string()
                .as_str()
            {
                "strict" => "strict",
                "relaxed" => "relaxed",
                "default" | "undefined" => "default",
                _ => w3cos_core::throw_value(dom_exception(
                    "TypeError",
                    "Transaction durability must be default, strict, or relaxed.",
                )),
            };
            let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
            match transaction_database.transaction(&refs, mode) {
                Ok(transaction) => transaction_web_value(
                    transaction,
                    transaction_connection.clone(),
                    names,
                    mode,
                    durability,
                ),
                Err(error) => w3cos_core::throw_value(dom_exception(&error.name, &error.message)),
            }
        }),
    );
    let close_connection_value = connection.clone();
    properties.insert(
        "close".into(),
        func(move |_, _| {
            close_connection(&close_connection_value);
            Value::Undefined
        }),
    );
    let value = Value::object(properties);
    if let Value::Object(object) = &value {
        *connection.target.borrow_mut() = Some(Rc::downgrade(object));
    }
    (value, connection)
}

fn object_store_schema_value(
    database: Database,
    store_name: String,
    upgrading: Rc<Cell<bool>>,
) -> Value {
    let (key_path, auto_increment) = database
        .object_store_definition(&store_name)
        .unwrap_or_else(|error| {
            w3cos_core::throw_value(dom_exception(&error.name, &error.message))
        });
    let names_database = database.clone();
    let names_store = store_name.clone();
    let mut properties = HashMap::from([
        ("name".into(), Value::from(store_name.as_str())),
        ("keyPath".into(), key_path_to_web(&key_path)),
        ("autoIncrement".into(), Value::Bool(auto_increment)),
        (
            "__w3cos_getter_indexNames".into(),
            func(move |_, _| {
                Value::array(
                    names_database
                        .index_names(&names_store)
                        .unwrap_or_default()
                        .into_iter()
                        .map(Value::from)
                        .collect(),
                )
            }),
        ),
    ]);
    let create_database = database.clone();
    let create_store = store_name.clone();
    let create_upgrading = upgrading.clone();
    properties.insert(
        "createIndex".into(),
        func(move |_, args| {
            if !create_upgrading.get() {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "Indexes can only be created during versionchange.",
                ));
            }
            let name = arg(&args, 0).to_js_string();
            let key_path = key_path_from_web(&arg(&args, 1), false).unwrap_or_else(|error| {
                w3cos_core::throw_value(dom_exception(&error.name, &error.message))
            });
            let options = arg(&args, 2);
            if let Err(error) = create_database.create_index_with_options(
                &create_store,
                name.clone(),
                key_path.clone(),
                options.get_property("unique").to_bool(),
                options.get_property("multiEntry").to_bool(),
            ) {
                w3cos_core::throw_value(dom_exception(&error.name, &error.message));
            }
            Value::object(HashMap::from([
                ("name".into(), Value::from(name)),
                ("keyPath".into(), key_path_to_web(&key_path)),
                (
                    "unique".into(),
                    Value::Bool(options.get_property("unique").to_bool()),
                ),
                (
                    "multiEntry".into(),
                    Value::Bool(options.get_property("multiEntry").to_bool()),
                ),
            ]))
        }),
    );
    properties.insert(
        "deleteIndex".into(),
        func(move |_, args| {
            if !upgrading.get() {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "Indexes can only be deleted during versionchange.",
                ));
            }
            if let Err(error) = database.delete_index(&store_name, &arg(&args, 0).to_js_string()) {
                w3cos_core::throw_value(dom_exception(&error.name, &error.message));
            }
            Value::Undefined
        }),
    );
    Value::object(properties)
}

fn transaction_web_value(
    transaction: Transaction,
    connection: Rc<DatabaseConnection>,
    names: Vec<String>,
    mode: TransactionMode,
    durability: &'static str,
) -> Value {
    let transaction = register_transaction(connection, names.clone(), mode, transaction);
    let mut properties = HashMap::from([
        (
            "mode".into(),
            Value::from(match mode {
                TransactionMode::ReadOnly => "readonly",
                TransactionMode::ReadWrite => "readwrite",
            }),
        ),
        ("durability".into(), Value::from(durability)),
        (
            "objectStoreNames".into(),
            Value::array(names.into_iter().map(Value::from).collect()),
        ),
        ("onabort".into(), Value::Null),
        ("oncomplete".into(), Value::Null),
        ("onerror".into(), Value::Null),
    ]);
    let transaction_error = transaction.clone();
    properties.insert(
        "__w3cos_getter_error".into(),
        func(move |_, _| transaction_error.error.borrow().clone()),
    );
    let add_listeners = transaction.listeners.clone();
    properties.insert(
        "addEventListener".into(),
        func(move |_, args| {
            let event_type = arg(&args, 0).to_js_string();
            let listener = arg(&args, 1);
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
    let remove_listeners = transaction.listeners.clone();
    properties.insert(
        "removeEventListener".into(),
        func(move |_, args| {
            let event_type = arg(&args, 0).to_js_string();
            let listener = arg(&args, 1);
            if let Some(entries) = remove_listeners.borrow_mut().get_mut(&event_type) {
                entries.retain(|candidate| !same_callback(candidate, &listener));
            }
            Value::Undefined
        }),
    );
    let object_store_transaction = transaction.clone();
    properties.insert(
        "objectStore".into(),
        func(move |_, args| {
            if object_store_transaction.commit_requested.get()
                || matches!(
                    object_store_transaction.state.get(),
                    WebTransactionState::Finished | WebTransactionState::Aborted
                )
            {
                w3cos_core::throw_value(dom_exception(
                    "TransactionInactiveError",
                    "The transaction is not active.",
                ));
            }
            let name = arg(&args, 0).to_js_string();
            match object_store_transaction.backend.object_store(&name) {
                Ok(_) => object_store_value(object_store_transaction.clone(), name),
                Err(error) => w3cos_core::throw_value(dom_exception(&error.name, &error.message)),
            }
        }),
    );
    let abort_transaction_value = transaction.clone();
    properties.insert(
        "abort".into(),
        func(move |_, _| {
            abort_transaction(&abort_transaction_value);
            Value::Undefined
        }),
    );
    let commit_transaction = transaction.clone();
    properties.insert(
        "commit".into(),
        func(move |_, _| {
            if matches!(
                commit_transaction.state.get(),
                WebTransactionState::Finished | WebTransactionState::Aborted
            ) {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "The transaction has already finished.",
                ));
            }
            commit_transaction.commit_requested.set(true);
            finish_transaction(&commit_transaction);
            Value::Undefined
        }),
    );
    let value = Value::object(properties);
    if let Value::Object(object) = &value {
        *transaction.target.borrow_mut() = Some(Rc::downgrade(object));
    }
    queue_transaction_finalize(transaction);
    value
}

fn object_store_value(transaction: Rc<WebTransaction>, store_name: String) -> Value {
    let (key_path, auto_increment, index_names) = transaction
        .backend
        .object_store(&store_name)
        .and_then(|store| store.definition())
        .unwrap_or_else(|error| {
            w3cos_core::throw_value(dom_exception(&error.name, &error.message))
        });
    let mut properties = HashMap::from([
        ("name".into(), Value::from(store_name.as_str())),
        ("keyPath".into(), key_path_to_web(&key_path)),
        ("autoIncrement".into(), Value::Bool(auto_increment)),
        (
            "indexNames".into(),
            Value::array(index_names.into_iter().map(Value::from).collect()),
        ),
    ]);

    let put_transaction = transaction.clone();
    let put_store = store_name.clone();
    properties.insert(
        "put".into(),
        func(move |_, args| {
            let value = arg(&args, 0);
            let key = arg(&args, 1);
            let transaction = put_transaction.clone();
            let store_name = put_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                let json = value_to_json(&value)?;
                let store = transaction.object_store(&store_name)?;
                let key = if key.is_undefined() {
                    store.put(json)?
                } else {
                    store.put_with_key(json, Some(value_to_json(&key)?))?
                };
                Ok(json_to_value(key))
            })
        }),
    );

    let add_transaction = transaction.clone();
    let add_store = store_name.clone();
    properties.insert(
        "add".into(),
        func(move |_, args| {
            let value = arg(&args, 0);
            let key = arg(&args, 1);
            let transaction = add_transaction.clone();
            let store_name = add_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                let json = value_to_json(&value)?;
                let store = transaction.object_store(&store_name)?;
                let key = if key.is_undefined() {
                    store.add(json)?
                } else {
                    store.add_with_key(json, Some(value_to_json(&key)?))?
                };
                Ok(json_to_value(key))
            })
        }),
    );

    let get_transaction = transaction.clone();
    let get_store = store_name.clone();
    properties.insert(
        "get".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let transaction = get_transaction.clone();
            let store_name = get_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                let store = transaction.object_store(&store_name)?;
                let range = key_range_from_query(&query)?.ok_or_else(|| IndexedDbError {
                    name: "DataError".into(),
                    message: "A key or key range is required.".into(),
                })?;
                Ok(store
                    .get_all_range(Some(&range), Some(1))?
                    .into_iter()
                    .next()
                    .map(json_to_value)
                    .unwrap_or(Value::Undefined))
            })
        }),
    );

    let all_transaction = transaction.clone();
    let all_store = store_name.clone();
    properties.insert(
        "getAll".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let count = arg(&args, 1);
            let transaction = all_transaction.clone();
            let store_name = all_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                let store = transaction.object_store(&store_name)?;
                let range = key_range_from_query(&query)?;
                let limit = if count.is_undefined() {
                    None
                } else {
                    Some(count.to_u32() as usize)
                };
                Ok(Value::array(
                    store
                        .get_all_range(range.as_ref(), limit)?
                        .into_iter()
                        .map(json_to_value)
                        .collect(),
                ))
            })
        }),
    );

    let keys_transaction = transaction.clone();
    let keys_store = store_name.clone();
    properties.insert(
        "getAllKeys".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let count = arg(&args, 1);
            let transaction = keys_transaction.clone();
            let store_name = keys_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                let store = transaction.object_store(&store_name)?;
                let range = key_range_from_query(&query)?;
                let limit = if count.is_undefined() {
                    None
                } else {
                    Some(count.to_u32() as usize)
                };
                Ok(Value::array(
                    store
                        .get_all_keys_range(range.as_ref(), limit)?
                        .into_iter()
                        .map(json_to_value)
                        .collect(),
                ))
            })
        }),
    );

    let count_transaction = transaction.clone();
    let count_store = store_name.clone();
    properties.insert(
        "count".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let transaction = count_transaction.clone();
            let store_name = count_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                let store = transaction.object_store(&store_name)?;
                let range = key_range_from_query(&query)?;
                Ok(Value::Number(store.count_range(range.as_ref())? as f64))
            })
        }),
    );

    let delete_transaction = transaction.clone();
    let delete_store = store_name.clone();
    properties.insert(
        "delete".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let transaction = delete_transaction.clone();
            let store_name = delete_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                let store = transaction.object_store(&store_name)?;
                let range = key_range_from_query(&query)?.ok_or_else(|| IndexedDbError {
                    name: "DataError".into(),
                    message: "A key or key range is required.".into(),
                })?;
                store.delete_range(&range)?;
                Ok(Value::Undefined)
            })
        }),
    );

    let clear_transaction = transaction.clone();
    let clear_store = store_name.clone();
    properties.insert(
        "clear".into(),
        func(move |_, _| {
            let transaction = clear_transaction.clone();
            let store_name = clear_store.clone();
            transaction_operation_request(transaction, move |transaction| {
                transaction.object_store(&store_name)?.clear()?;
                Ok(Value::Undefined)
            })
        }),
    );

    let cursor_transaction = transaction.clone();
    let cursor_store = store_name.clone();
    properties.insert(
        "openCursor".into(),
        func(move |_, args| {
            cursor_request(
                cursor_transaction.clone(),
                cursor_store.clone(),
                arg(&args, 0),
                arg(&args, 1),
                false,
            )
        }),
    );

    let key_cursor_transaction = transaction.clone();
    let key_cursor_store = store_name.clone();
    properties.insert(
        "openKeyCursor".into(),
        func(move |_, args| {
            cursor_request(
                key_cursor_transaction.clone(),
                key_cursor_store.clone(),
                arg(&args, 0),
                arg(&args, 1),
                true,
            )
        }),
    );

    let index_transaction = transaction;
    properties.insert(
        "index".into(),
        func(move |_, args| {
            index_value(
                index_transaction.clone(),
                store_name.clone(),
                arg(&args, 0).to_js_string(),
            )
        }),
    );
    Value::object(properties)
}

fn cursor_request(
    transaction: Rc<WebTransaction>,
    store_name: String,
    query: Value,
    direction: Value,
    key_only: bool,
) -> Value {
    if transaction.commit_requested.get()
        || matches!(
            transaction.state.get(),
            WebTransactionState::Finished | WebTransactionState::Aborted
        )
    {
        w3cos_core::throw_value(dom_exception(
            "TransactionInactiveError",
            "The transaction is not active.",
        ));
    }
    let range = match key_range_from_query(&query) {
        Ok(range) => range,
        Err(error) => w3cos_core::throw_value(dom_exception(&error.name, &error.message)),
    };
    let direction = cursor_direction(&direction);
    transaction
        .pending_requests
        .set(transaction.pending_requests.get() + 1);
    let (request, request_state, listeners) = request_value();
    request_state.borrow_mut().transaction = transaction.target_value().unwrap_or(Value::Null);
    queue_cursor_initial(
        transaction,
        store_name,
        range,
        direction,
        key_only,
        request.clone(),
        request_state,
        listeners,
    );
    request
}

fn cursor_direction(direction: &Value) -> &'static str {
    if direction.is_undefined() {
        return "next";
    }
    match direction.to_js_string().as_str() {
        "next" => "next",
        "nextunique" => "nextunique",
        "prev" => "prev",
        "prevunique" => "prevunique",
        _ => w3cos_core::throw_value(dom_exception(
            "TypeError",
            "Cursor direction must be next, nextunique, prev, or prevunique.",
        )),
    }
}

fn index_cursor_request(
    transaction: Rc<WebTransaction>,
    store_name: String,
    index_name: String,
    query: Value,
    direction: Value,
    key_only: bool,
) -> Value {
    if transaction.commit_requested.get()
        || matches!(
            transaction.state.get(),
            WebTransactionState::Finished | WebTransactionState::Aborted
        )
    {
        w3cos_core::throw_value(dom_exception(
            "TransactionInactiveError",
            "The transaction is not active.",
        ));
    }
    let range = match key_range_from_query(&query) {
        Ok(range) => range,
        Err(error) => w3cos_core::throw_value(dom_exception(&error.name, &error.message)),
    };
    let direction = cursor_direction(&direction);
    transaction
        .pending_requests
        .set(transaction.pending_requests.get() + 1);
    let (request, request_state, listeners) = request_value();
    request_state.borrow_mut().transaction = transaction.target_value().unwrap_or(Value::Null);
    queue_index_cursor_initial(
        transaction,
        store_name,
        index_name,
        range,
        direction,
        key_only,
        request.clone(),
        request_state,
        listeners,
    );
    request
}

#[allow(clippy::too_many_arguments)]
fn queue_index_cursor_initial(
    transaction: Rc<WebTransaction>,
    store_name: String,
    index_name: String,
    range: Option<KeyRange>,
    direction: &'static str,
    key_only: bool,
    request: Value,
    request_state: Rc<RefCell<RequestState>>,
    listeners: ListenerMap,
) {
    queue_microtask_value(func(move |_, _| {
        match transaction.state.get() {
            WebTransactionState::Queued => queue_index_cursor_initial(
                transaction.clone(),
                store_name.clone(),
                index_name.clone(),
                range.clone(),
                direction,
                key_only,
                request.clone(),
                request_state.clone(),
                listeners.clone(),
            ),
            WebTransactionState::Active => {
                match transaction
                    .backend
                    .object_store(&store_name)
                    .and_then(|store| store.index_scan(&index_name, range.as_ref()))
                {
                    Ok(mut entries) => {
                        if direction.starts_with("prev") {
                            entries.reverse();
                        }
                        if direction.ends_with("unique") {
                            let mut seen = std::collections::HashSet::new();
                            entries.retain(|(key, _, _)| {
                                indexed_db::canonicalize_key(key)
                                    .ok()
                                    .is_some_and(|key| seen.insert(key))
                            });
                        }
                        let entries = entries
                            .into_iter()
                            .map(|(key, primary_key, value)| CursorEntry {
                                key,
                                primary_key,
                                value,
                            })
                            .collect();
                        let cursor = Rc::new(CursorState {
                            transaction: transaction.clone(),
                            store_name: store_name.clone(),
                            entries,
                            position: Cell::new(0),
                            direction,
                            key_only,
                            request: request.clone(),
                            request_state: request_state.clone(),
                            listeners: listeners.clone(),
                            advancing: Cell::new(false),
                        });
                        emit_cursor_result(&cursor);
                    }
                    Err(error) => {
                        let _ = complete_error(&request, &request_state, &listeners, error);
                    }
                }
                complete_cursor_step(&transaction);
            }
            WebTransactionState::Finished | WebTransactionState::Aborted => {
                let _ = complete_error(
                    &request,
                    &request_state,
                    &listeners,
                    IndexedDbError {
                        name: "TransactionInactiveError".into(),
                        message: "The transaction is not active.".into(),
                    },
                );
                complete_cursor_step(&transaction);
            }
        }
        Value::Undefined
    }));
}

fn queue_cursor_initial(
    transaction: Rc<WebTransaction>,
    store_name: String,
    range: Option<KeyRange>,
    direction: &'static str,
    key_only: bool,
    request: Value,
    request_state: Rc<RefCell<RequestState>>,
    listeners: ListenerMap,
) {
    queue_microtask_value(func(move |_, _| {
        match transaction.state.get() {
            WebTransactionState::Queued => queue_cursor_initial(
                transaction.clone(),
                store_name.clone(),
                range.clone(),
                direction,
                key_only,
                request.clone(),
                request_state.clone(),
                listeners.clone(),
            ),
            WebTransactionState::Active => {
                match transaction
                    .backend
                    .object_store(&store_name)
                    .and_then(|store| store.scan_range(range.as_ref()))
                {
                    Ok(mut entries) => {
                        if direction.starts_with("prev") {
                            entries.reverse();
                        }
                        let entries = entries
                            .into_iter()
                            .map(|(key, value)| CursorEntry {
                                primary_key: key.clone(),
                                key,
                                value,
                            })
                            .collect();
                        let cursor = Rc::new(CursorState {
                            transaction: transaction.clone(),
                            store_name: store_name.clone(),
                            entries,
                            position: Cell::new(0),
                            direction,
                            key_only,
                            request: request.clone(),
                            request_state: request_state.clone(),
                            listeners: listeners.clone(),
                            advancing: Cell::new(false),
                        });
                        emit_cursor_result(&cursor);
                    }
                    Err(error) => {
                        let _ = complete_error(&request, &request_state, &listeners, error);
                    }
                }
                complete_cursor_step(&transaction);
            }
            WebTransactionState::Finished | WebTransactionState::Aborted => {
                let _ = complete_error(
                    &request,
                    &request_state,
                    &listeners,
                    IndexedDbError {
                        name: "TransactionInactiveError".into(),
                        message: "The transaction is not active.".into(),
                    },
                );
                complete_cursor_step(&transaction);
            }
        }
        Value::Undefined
    }));
}

fn complete_cursor_step(transaction: &Rc<WebTransaction>) {
    transaction
        .pending_requests
        .set(transaction.pending_requests.get().saturating_sub(1));
    finish_transaction(transaction);
}

fn emit_cursor_result(cursor: &Rc<CursorState>) {
    cursor.advancing.set(false);
    let result = if cursor.position.get() >= cursor.entries.len() {
        Value::Null
    } else {
        cursor_value(cursor.clone())
    };
    complete_success(
        &cursor.request,
        &cursor.request_state,
        &cursor.listeners,
        result,
    );
}

fn queue_cursor_continue(cursor: Rc<CursorState>, amount: usize) {
    cursor.advancing.set(true);
    cursor
        .transaction
        .pending_requests
        .set(cursor.transaction.pending_requests.get() + 1);
    {
        let mut request = cursor.request_state.borrow_mut();
        request.ready_state = "pending";
        request.result_available = false;
        request.result = Value::Undefined;
    }
    queue_microtask_value(func(move |_, _| {
        if cursor.transaction.state.get() == WebTransactionState::Active {
            cursor
                .position
                .set(cursor.position.get().saturating_add(amount));
            emit_cursor_result(&cursor);
        } else {
            let _ = complete_error(
                &cursor.request,
                &cursor.request_state,
                &cursor.listeners,
                IndexedDbError {
                    name: "TransactionInactiveError".into(),
                    message: "The transaction is not active.".into(),
                },
            );
        }
        complete_cursor_step(&cursor.transaction);
        Value::Undefined
    }));
}

fn cursor_value(cursor: Rc<CursorState>) -> Value {
    let entry = &cursor.entries[cursor.position.get()];
    let key = entry.key.clone();
    let primary_key = entry.primary_key.clone();
    let value = entry.value.clone();
    let mut properties = HashMap::from([
        ("direction".into(), Value::from(cursor.direction)),
        ("key".into(), json_to_value(key.clone())),
        ("primaryKey".into(), json_to_value(primary_key.clone())),
    ]);
    if !cursor.key_only {
        properties.insert("value".into(), json_to_value(value));
    }
    let continue_cursor = cursor.clone();
    properties.insert(
        "continue".into(),
        func(move |_, args| {
            if continue_cursor.advancing.get() {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "The cursor is already advancing.",
                ));
            }
            let target = arg(&args, 0);
            let amount = if target.is_undefined() {
                1
            } else {
                match cursor_continue_amount(&continue_cursor, &target) {
                    Ok(amount) => amount,
                    Err(error) => {
                        w3cos_core::throw_value(dom_exception(&error.name, &error.message))
                    }
                }
            };
            queue_cursor_continue(continue_cursor.clone(), amount);
            Value::Undefined
        }),
    );
    let advance_cursor = cursor.clone();
    properties.insert(
        "advance".into(),
        func(move |_, args| {
            let amount = arg(&args, 0).to_u32() as usize;
            if amount == 0 {
                w3cos_core::throw_value(dom_exception(
                    "TypeError",
                    "Cursor advance count must be greater than zero.",
                ));
            }
            if advance_cursor.advancing.get() {
                w3cos_core::throw_value(dom_exception(
                    "InvalidStateError",
                    "The cursor is already advancing.",
                ));
            }
            queue_cursor_continue(advance_cursor.clone(), amount);
            Value::Undefined
        }),
    );
    if !cursor.key_only {
        let update_cursor = cursor.clone();
        let update_key = primary_key.clone();
        properties.insert(
            "update".into(),
            func(move |_, args| {
                let value = arg(&args, 0);
                let transaction = update_cursor.transaction.clone();
                let store_name = update_cursor.store_name.clone();
                let key = update_key.clone();
                transaction_operation_request(transaction, move |transaction| {
                    let result = transaction
                        .object_store(&store_name)?
                        .put_with_key(value_to_json(&value)?, Some(key))?;
                    Ok(json_to_value(result))
                })
            }),
        );
        let delete_cursor = cursor.clone();
        let delete_key = primary_key;
        properties.insert(
            "delete".into(),
            func(move |_, _| {
                let transaction = delete_cursor.transaction.clone();
                let store_name = delete_cursor.store_name.clone();
                let key = delete_key.clone();
                transaction_operation_request(transaction, move |transaction| {
                    transaction.object_store(&store_name)?.delete(&key)?;
                    Ok(Value::Undefined)
                })
            }),
        );
    }
    Value::object(properties)
}

fn cursor_continue_amount(cursor: &CursorState, target: &Value) -> indexed_db::Result<usize> {
    let target = value_to_json(target)?;
    let current = &cursor.entries[cursor.position.get()].key;
    let ordering = indexed_db::compare_keys(&target, current)?;
    let reverse = cursor.direction.starts_with("prev");
    if (!reverse && ordering != Ordering::Greater) || (reverse && ordering != Ordering::Less) {
        return Err(IndexedDbError {
            name: "DataError".into(),
            message: "The continue key must advance in the cursor direction.".into(),
        });
    }
    let remaining = cursor.entries.iter().skip(cursor.position.get() + 1);
    let offset = remaining
        .enumerate()
        .find_map(|(offset, entry)| {
            let ordering = indexed_db::compare_keys(&entry.key, &target).ok()?;
            ((!reverse && ordering != Ordering::Less) || (reverse && ordering != Ordering::Greater))
                .then_some(offset + 1)
        })
        .unwrap_or_else(|| cursor.entries.len().saturating_sub(cursor.position.get()));
    Ok(offset)
}

fn index_value(transaction: Rc<WebTransaction>, store_name: String, index_name: String) -> Value {
    let (key_path, unique, multi_entry) = transaction
        .backend
        .object_store(&store_name)
        .and_then(|store| store.index_definition(&index_name))
        .unwrap_or_else(|error| {
            w3cos_core::throw_value(dom_exception(&error.name, &error.message))
        });
    let mut properties = HashMap::from([
        ("name".into(), Value::from(index_name.as_str())),
        ("keyPath".into(), key_path_to_web(&key_path)),
        ("unique".into(), Value::Bool(unique)),
        ("multiEntry".into(), Value::Bool(multi_entry)),
    ]);

    let get_transaction = transaction.clone();
    let get_store = store_name.clone();
    let get_index = index_name.clone();
    properties.insert(
        "get".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let transaction = get_transaction.clone();
            let store_name = get_store.clone();
            let index_name = get_index.clone();
            transaction_operation_request(transaction, move |transaction| {
                let range = key_range_from_query(&query)?.ok_or_else(|| IndexedDbError {
                    name: "DataError".into(),
                    message: "A key or key range is required.".into(),
                })?;
                Ok(transaction
                    .object_store(&store_name)?
                    .index_scan(&index_name, Some(&range))?
                    .into_iter()
                    .next()
                    .map(|(_, _, value)| json_to_value(value))
                    .unwrap_or(Value::Undefined))
            })
        }),
    );

    let get_key_transaction = transaction.clone();
    let get_key_store = store_name.clone();
    let get_key_index = index_name.clone();
    properties.insert(
        "getKey".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let transaction = get_key_transaction.clone();
            let store_name = get_key_store.clone();
            let index_name = get_key_index.clone();
            transaction_operation_request(transaction, move |transaction| {
                let range = key_range_from_query(&query)?.ok_or_else(|| IndexedDbError {
                    name: "DataError".into(),
                    message: "A key or key range is required.".into(),
                })?;
                Ok(transaction
                    .object_store(&store_name)?
                    .index_scan(&index_name, Some(&range))?
                    .into_iter()
                    .next()
                    .map(|(_, primary_key, _)| json_to_value(primary_key))
                    .unwrap_or(Value::Undefined))
            })
        }),
    );

    let all_transaction = transaction.clone();
    let all_store = store_name.clone();
    let all_index = index_name.clone();
    properties.insert(
        "getAll".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let count = arg(&args, 1);
            let transaction = all_transaction.clone();
            let store_name = all_store.clone();
            let index_name = all_index.clone();
            transaction_operation_request(transaction, move |transaction| {
                let range = key_range_from_query(&query)?;
                let limit = if count.is_undefined() {
                    usize::MAX
                } else {
                    count.to_u32() as usize
                };
                Ok(Value::array(
                    transaction
                        .object_store(&store_name)?
                        .index_scan(&index_name, range.as_ref())?
                        .into_iter()
                        .take(limit)
                        .map(|(_, _, value)| json_to_value(value))
                        .collect(),
                ))
            })
        }),
    );

    let keys_transaction = transaction.clone();
    let keys_store = store_name.clone();
    let keys_index = index_name.clone();
    properties.insert(
        "getAllKeys".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let count = arg(&args, 1);
            let transaction = keys_transaction.clone();
            let store_name = keys_store.clone();
            let index_name = keys_index.clone();
            transaction_operation_request(transaction, move |transaction| {
                let range = key_range_from_query(&query)?;
                let limit = if count.is_undefined() {
                    usize::MAX
                } else {
                    count.to_u32() as usize
                };
                Ok(Value::array(
                    transaction
                        .object_store(&store_name)?
                        .index_scan(&index_name, range.as_ref())?
                        .into_iter()
                        .take(limit)
                        .map(|(_, primary_key, _)| json_to_value(primary_key))
                        .collect(),
                ))
            })
        }),
    );

    let count_transaction = transaction.clone();
    let count_store = store_name.clone();
    let count_index = index_name.clone();
    properties.insert(
        "count".into(),
        func(move |_, args| {
            let query = arg(&args, 0);
            let transaction = count_transaction.clone();
            let store_name = count_store.clone();
            let index_name = count_index.clone();
            transaction_operation_request(transaction, move |transaction| {
                let range = key_range_from_query(&query)?;
                Ok(Value::Number(
                    transaction
                        .object_store(&store_name)?
                        .index_scan(&index_name, range.as_ref())?
                        .len() as f64,
                ))
            })
        }),
    );

    let cursor_transaction = transaction.clone();
    let cursor_store = store_name.clone();
    let cursor_index = index_name.clone();
    properties.insert(
        "openCursor".into(),
        func(move |_, args| {
            index_cursor_request(
                cursor_transaction.clone(),
                cursor_store.clone(),
                cursor_index.clone(),
                arg(&args, 0),
                arg(&args, 1),
                false,
            )
        }),
    );

    properties.insert(
        "openKeyCursor".into(),
        func(move |_, args| {
            index_cursor_request(
                transaction.clone(),
                store_name.clone(),
                index_name.clone(),
                arg(&args, 0),
                arg(&args, 1),
                true,
            )
        }),
    );
    Value::object(properties)
}

type TransactionOperation = Box<dyn FnOnce(&Transaction) -> indexed_db::Result<Value>>;

fn transaction_operation_request(
    transaction: Rc<WebTransaction>,
    operation: impl FnOnce(&Transaction) -> indexed_db::Result<Value> + 'static,
) -> Value {
    if transaction.commit_requested.get()
        || matches!(
            transaction.state.get(),
            WebTransactionState::Finished | WebTransactionState::Aborted
        )
    {
        w3cos_core::throw_value(dom_exception(
            "TransactionInactiveError",
            "The transaction is not active.",
        ));
    }
    transaction
        .pending_requests
        .set(transaction.pending_requests.get() + 1);
    let (request, state, listeners) = request_value();
    state.borrow_mut().transaction = transaction.target_value().unwrap_or(Value::Null);
    let operation: Rc<RefCell<Option<TransactionOperation>>> =
        Rc::new(RefCell::new(Some(Box::new(operation))));
    queue_transaction_operation(transaction, request.clone(), state, listeners, operation);
    request
}

fn queue_transaction_operation(
    transaction: Rc<WebTransaction>,
    request: Value,
    state: Rc<RefCell<RequestState>>,
    listeners: ListenerMap,
    operation: Rc<RefCell<Option<TransactionOperation>>>,
) {
    queue_microtask_value(func(move |_, _| {
        match transaction.state.get() {
            WebTransactionState::Queued => {
                queue_transaction_operation(
                    transaction.clone(),
                    request.clone(),
                    state.clone(),
                    listeners.clone(),
                    operation.clone(),
                );
            }
            WebTransactionState::Active => {
                let Some(operation) = operation.borrow_mut().take() else {
                    return Value::Undefined;
                };
                match operation(&transaction.backend) {
                    Ok(result) => complete_success(&request, &state, &listeners, result),
                    Err(error) => {
                        let transaction_error = dom_exception(&error.name, &error.message);
                        *transaction.error.borrow_mut() = transaction_error;
                        let mut prevented = complete_error(&request, &state, &listeners, error);
                        prevented |= fire_transaction_error(&transaction);
                        prevented |= fire_database_error(&transaction.connection);
                        if !prevented {
                            let _ = transaction.backend.abort();
                            transaction.state.set(WebTransactionState::Aborted);
                            fire_transaction_event(&transaction, "abort");
                            release_transaction(&transaction);
                        }
                    }
                }
                transaction
                    .pending_requests
                    .set(transaction.pending_requests.get().saturating_sub(1));
                finish_transaction(&transaction);
            }
            WebTransactionState::Finished | WebTransactionState::Aborted => {
                let _ = operation.borrow_mut().take();
                let _ = complete_error(
                    &request,
                    &state,
                    &listeners,
                    IndexedDbError {
                        name: "TransactionInactiveError".into(),
                        message: "The transaction is not active.".into(),
                    },
                );
                transaction
                    .pending_requests
                    .set(transaction.pending_requests.get().saturating_sub(1));
            }
        }
        Value::Undefined
    }));
}

const CLONE_TAG: &str = "\u{1f}w3cos-idb-clone";
const DATE_TAG: &str = "\u{1f}w3cos-idb-date";
const BINARY_TAG: &str = "\u{1f}w3cos-idb-binary";

fn value_to_json(value: &Value) -> indexed_db::Result<JsonValue> {
    match value_to_json_inner(value, &mut std::collections::HashSet::new()) {
        Err(error) if error.name == "DataCloneError" && error.message.starts_with("Cyclic") => {
            graph_clone_to_json(value)
        }
        result => result,
    }
}

struct GraphCloneEncoder {
    ids: HashMap<(u8, usize), usize>,
    nodes: Vec<JsonValue>,
}

fn graph_clone_to_json(value: &Value) -> indexed_db::Result<JsonValue> {
    let mut encoder = GraphCloneEncoder {
        ids: HashMap::new(),
        nodes: Vec::new(),
    };
    let root = encoder.encode(value)?;
    Ok(JsonValue::Object(serde_json::Map::from_iter([
        (CLONE_TAG.into(), JsonValue::String("graph".into())),
        ("root".into(), root),
        ("nodes".into(), JsonValue::Array(encoder.nodes)),
    ])))
}

impl GraphCloneEncoder {
    fn encode(&mut self, value: &Value) -> indexed_db::Result<JsonValue> {
        match value {
            Value::Array(values) if !w3cos_core::collections::is_typed_array(value) => {
                let identity = (0, Rc::as_ptr(values) as usize);
                if let Some(id) = self.ids.get(&identity) {
                    return Ok(graph_reference(*id));
                }
                let id = self.nodes.len();
                self.ids.insert(identity, id);
                self.nodes.push(JsonValue::Null);
                let items = values
                    .borrow()
                    .iter()
                    .map(|value| self.encode(value))
                    .collect::<indexed_db::Result<Vec<_>>>()?;
                self.nodes[id] = JsonValue::Object(serde_json::Map::from_iter([
                    ("kind".into(), JsonValue::String("array".into())),
                    ("value".into(), JsonValue::Array(items)),
                ]));
                Ok(graph_reference(id))
            }
            Value::Object(object)
                if !value.get_property("__w3cos_date_milliseconds").is_number() =>
            {
                let identity = (1, Rc::as_ptr(object) as usize);
                if let Some(id) = self.ids.get(&identity) {
                    return Ok(graph_reference(*id));
                }
                let id = self.nodes.len();
                self.ids.insert(identity, id);
                self.nodes.push(JsonValue::Null);
                let object = object.borrow();
                let properties = object
                    .keys()
                    .into_iter()
                    .map(|key| {
                        self.encode(&object.get_direct(&key))
                            .map(|value| (key, value))
                    })
                    .collect::<indexed_db::Result<serde_json::Map<_, _>>>()?;
                self.nodes[id] = JsonValue::Object(serde_json::Map::from_iter([
                    ("kind".into(), JsonValue::String("object".into())),
                    ("value".into(), JsonValue::Object(properties)),
                ]));
                Ok(graph_reference(id))
            }
            _ => value_to_json_inner(value, &mut std::collections::HashSet::new()),
        }
    }
}

fn graph_reference(id: usize) -> JsonValue {
    JsonValue::Object(serde_json::Map::from_iter([
        (CLONE_TAG.into(), JsonValue::String("ref".into())),
        ("id".into(), JsonValue::Number((id as u64).into())),
    ]))
}

fn value_to_json_inner(
    value: &Value,
    active: &mut std::collections::HashSet<usize>,
) -> indexed_db::Result<JsonValue> {
    match value {
        Value::Undefined => Ok(JsonValue::Object(serde_json::Map::from_iter([(
            CLONE_TAG.into(),
            JsonValue::String("undefined".into()),
        )]))),
        Value::Function(_) => Err(IndexedDbError {
            name: "DataCloneError".into(),
            message: "The value cannot be structured cloned.".into(),
        }),
        Value::Null => Ok(JsonValue::Null),
        Value::Bool(value) => Ok(JsonValue::Bool(*value)),
        Value::Number(value) => serde_json::Number::from_f64(*value)
            .map(JsonValue::Number)
            .map_or_else(
                || {
                    Ok(JsonValue::Object(serde_json::Map::from_iter([
                        (CLONE_TAG.into(), JsonValue::String("number".into())),
                        (
                            "value".into(),
                            JsonValue::String(
                                if value.is_nan() {
                                    "NaN"
                                } else if value.is_sign_positive() {
                                    "Infinity"
                                } else {
                                    "-Infinity"
                                }
                                .into(),
                            ),
                        ),
                    ])))
                },
                Ok,
            ),
        Value::String(value) => Ok(JsonValue::String(value.clone())),
        Value::Array(values) => {
            if w3cos_core::collections::is_typed_array(value) {
                let bytes = values
                    .borrow()
                    .iter()
                    .map(|value| {
                        let number = value.to_number();
                        if !number.is_finite()
                            || number.fract() != 0.0
                            || !(0.0..=255.0).contains(&number)
                        {
                            return Err(IndexedDbError {
                                name: "DataCloneError".into(),
                                message: "The compact typed-array codec requires byte values."
                                    .into(),
                            });
                        }
                        Ok(JsonValue::Number(serde_json::Number::from(number as u8)))
                    })
                    .collect::<indexed_db::Result<Vec<_>>>()?;
                return Ok(JsonValue::Object(serde_json::Map::from_iter([(
                    BINARY_TAG.into(),
                    JsonValue::Array(bytes),
                )])));
            }
            let pointer = Rc::as_ptr(values) as usize;
            if !active.insert(pointer) {
                return Err(IndexedDbError {
                    name: "DataCloneError".into(),
                    message: "Cyclic values are not yet supported by this storage codec.".into(),
                });
            }
            let result = values
                .borrow()
                .iter()
                .map(|value| value_to_json_inner(value, active))
                .collect::<indexed_db::Result<Vec<_>>>()
                .map(JsonValue::Array);
            active.remove(&pointer);
            result
        }
        Value::Object(object) => {
            let date = value.get_property("__w3cos_date_milliseconds");
            if date.is_number() {
                let milliseconds = date.to_number();
                if !milliseconds.is_finite() {
                    return Err(IndexedDbError {
                        name: "DataCloneError".into(),
                        message: "Invalid Date values cannot be stored as IndexedDB keys.".into(),
                    });
                }
                return Ok(JsonValue::Object(serde_json::Map::from_iter([(
                    DATE_TAG.into(),
                    JsonValue::Number(
                        serde_json::Number::from_f64(milliseconds)
                            .expect("finite date milliseconds"),
                    ),
                )])));
            }
            let pointer = Rc::as_ptr(object) as usize;
            if !active.insert(pointer) {
                return Err(IndexedDbError {
                    name: "DataCloneError".into(),
                    message: "Cyclic values are not yet supported by this storage codec.".into(),
                });
            }
            let object = object.borrow();
            let mut cloned = object
                .keys()
                .into_iter()
                .map(|key| {
                    value_to_json_inner(&object.get_direct(&key), active).map(|value| (key, value))
                })
                .collect::<indexed_db::Result<serde_json::Map<_, _>>>()?;
            active.remove(&pointer);
            if cloned.contains_key(CLONE_TAG)
                || cloned.contains_key(DATE_TAG)
                || cloned.contains_key(BINARY_TAG)
            {
                cloned = serde_json::Map::from_iter([
                    (CLONE_TAG.into(), JsonValue::String("object".into())),
                    ("value".into(), JsonValue::Object(cloned)),
                ]);
            }
            Ok(JsonValue::Object(cloned))
        }
    }
}

fn json_to_value(value: JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Bool(value),
        JsonValue::Number(value) => Value::Number(value.as_f64().unwrap_or(f64::NAN)),
        JsonValue::String(value) => Value::String(value),
        JsonValue::Array(values) => Value::array(values.into_iter().map(json_to_value).collect()),
        JsonValue::Object(mut values) => {
            if let Some(milliseconds) = values.get(DATE_TAG).and_then(JsonValue::as_f64) {
                return w3cos_core::web::date_value(milliseconds);
            }
            if let Some(bytes) = values.get(BINARY_TAG).and_then(JsonValue::as_array) {
                return w3cos_core::collections::typed_array_value(
                    bytes
                        .iter()
                        .filter_map(JsonValue::as_u64)
                        .map(|byte| Value::Number(byte as f64))
                        .collect(),
                );
            }
            match values.get(CLONE_TAG).and_then(JsonValue::as_str) {
                Some("graph") => return json_graph_to_value(&values),
                Some("undefined") => return Value::Undefined,
                Some("number") => {
                    return Value::Number(match values.get("value").and_then(JsonValue::as_str) {
                        Some("NaN") => f64::NAN,
                        Some("Infinity") => f64::INFINITY,
                        Some("-Infinity") => f64::NEG_INFINITY,
                        _ => f64::NAN,
                    });
                }
                Some("object") => {
                    if let Some(JsonValue::Object(original)) = values.remove("value") {
                        values = original;
                    }
                }
                _ => {}
            }
            Value::object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, json_to_value(value)))
                    .collect(),
            )
        }
    }
}

fn json_graph_to_value(graph: &serde_json::Map<String, JsonValue>) -> Value {
    let Some(nodes) = graph.get("nodes").and_then(JsonValue::as_array) else {
        return Value::Undefined;
    };
    let placeholders = nodes
        .iter()
        .map(|node| match node.get("kind").and_then(JsonValue::as_str) {
            Some("array") => Value::array(Vec::new()),
            _ => Value::object(HashMap::new()),
        })
        .collect::<Vec<_>>();
    for (index, node) in nodes.iter().enumerate() {
        match node.get("kind").and_then(JsonValue::as_str) {
            Some("array") => {
                let items = node
                    .get("value")
                    .and_then(JsonValue::as_array)
                    .into_iter()
                    .flatten()
                    .map(|value| json_graph_part_to_value(value, &placeholders))
                    .collect::<Vec<_>>();
                if let Value::Array(storage) = &placeholders[index] {
                    *storage.borrow_mut() = items;
                }
            }
            Some("object") => {
                if let Some(properties) = node.get("value").and_then(JsonValue::as_object) {
                    for (key, value) in properties {
                        placeholders[index]
                            .set_property(key, json_graph_part_to_value(value, &placeholders));
                    }
                }
            }
            _ => {}
        }
    }
    graph
        .get("root")
        .map(|root| json_graph_part_to_value(root, &placeholders))
        .unwrap_or(Value::Undefined)
}

fn json_graph_part_to_value(value: &JsonValue, placeholders: &[Value]) -> Value {
    if value.get(CLONE_TAG).and_then(JsonValue::as_str) == Some("ref") {
        return value
            .get("id")
            .and_then(JsonValue::as_u64)
            .and_then(|id| placeholders.get(id as usize))
            .cloned()
            .unwrap_or(Value::Undefined);
    }
    json_to_value(value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("w3cos-idb-web-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn standard_open_upgrade_put_and_get_are_request_driven() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("crud"));
        let factory = factory_value();
        let open = factory.call_method("open", vec![Value::from("app"), Value::Number(1.0)]);
        assert_eq!(open.get_property("readyState").to_js_string(), "pending");

        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                let database = upgrade_open.get_property("result");
                let store = database.call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                store.call_method(
                    "createIndex",
                    vec![Value::from("by_kind"), Value::from("kind")],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(open.get_property("readyState").to_js_string(), "done");

        let database = open.get_property("result");
        assert_eq!(database.get_property("version").to_u32(), 1);
        assert_eq!(database.get_property("objectStoreNames").iter().count(), 1);
        let transaction = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let store = transaction.call_method("objectStore", vec![Value::from("items")]);
        let put = store.call_method(
            "put",
            vec![Value::object(HashMap::from([
                ("id".into(), Value::from("one")),
                ("kind".into(), Value::from("offline")),
                ("text".into(), Value::from("offline")),
            ]))],
        );
        store.call_method(
            "add",
            vec![Value::object(HashMap::from([
                ("id".into(), Value::from("two")),
                ("kind".into(), Value::from("offline")),
                ("text".into(), Value::from("queued")),
            ]))],
        );
        assert_eq!(put.get_property("readyState").to_js_string(), "pending");
        crate::jsdom::drain_microtasks();
        assert_eq!(put.get_property("result").to_js_string(), "one");

        let read_transaction = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let read_store = read_transaction.call_method("objectStore", vec![Value::from("items")]);
        let get = read_store.call_method("get", vec![Value::from("one")]);
        let count = read_store.call_method("count", vec![]);
        let keys = read_store.call_method("getAllKeys", vec![]);
        let indexed = read_store
            .call_method("index", vec![Value::from("by_kind")])
            .call_method("getAll", vec![Value::from("offline")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            get.get_property("result")
                .get_property("text")
                .to_js_string(),
            "offline"
        );
        assert_eq!(count.get_property("result").to_u32(), 2);
        assert_eq!(keys.get_property("result").iter().count(), 2);
        assert_eq!(indexed.get_property("result").iter().count(), 2);

        let cleanup = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let cleanup_store = cleanup.call_method("objectStore", vec![Value::from("items")]);
        cleanup_store.call_method("delete", vec![Value::from("one")]);
        let clear = cleanup_store.call_method("clear", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(clear.get_property("error"), Value::Null);
    }

    #[test]
    fn omitted_version_reuses_current_and_older_version_fails_with_version_error() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("version"));
        let factory = factory_value();
        let create =
            factory.call_method("open", vec![Value::from("versioned"), Value::Number(2.0)]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            create
                .get_property("result")
                .get_property("version")
                .to_u32(),
            2
        );

        let reopen = factory.call_method("open", vec![Value::from("versioned")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(reopen.get_property("error"), Value::Null);
        assert_eq!(
            reopen
                .get_property("result")
                .get_property("version")
                .to_u32(),
            2
        );

        let stale = factory.call_method("open", vec![Value::from("versioned"), Value::Number(1.0)]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            stale
                .get_property("error")
                .get_property("name")
                .to_js_string(),
            "VersionError"
        );
    }

    #[test]
    fn overlapping_read_waits_for_the_earlier_atomic_write_scope() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("scope-order"));
        let factory = factory_value();
        let open = factory.call_method("open", vec![Value::from("ordered"), Value::Number(1.0)]);
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");

        let write = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let complete_count = Rc::new(Cell::new(0));
        let observed_complete = complete_count.clone();
        write.set_property(
            "oncomplete",
            func(move |_, _| {
                observed_complete.set(observed_complete.get() + 1);
                Value::Undefined
            }),
        );
        let write_store = write.call_method("objectStore", vec![Value::from("items")]);
        write_store.call_method(
            "put",
            vec![Value::object(HashMap::from([(
                "id".into(),
                Value::from("one"),
            )]))],
        );

        // Created between two requests from the earlier write transaction.
        // It must not start until the complete write scope commits.
        let read = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let read_store = read.call_method("objectStore", vec![Value::from("items")]);
        let all = read_store.call_method("getAll", vec![]);

        write_store.call_method(
            "put",
            vec![Value::object(HashMap::from([(
                "id".into(),
                Value::from("two"),
            )]))],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(all.get_property("result").iter().count(), 2);
        assert_eq!(complete_count.get(), 1);
    }

    #[test]
    fn abort_rolls_back_pending_web_requests_and_emits_abort() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("web-abort"));
        let factory = factory_value();
        let open = factory.call_method("open", vec![Value::from("abort-web"), Value::Number(1.0)]);
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");
        let write = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let abort_count = Rc::new(Cell::new(0));
        let observed_abort = abort_count.clone();
        write.set_property(
            "onabort",
            func(move |_, _| {
                observed_abort.set(observed_abort.get() + 1);
                Value::Undefined
            }),
        );
        write
            .call_method("objectStore", vec![Value::from("items")])
            .call_method(
                "put",
                vec![Value::object(HashMap::from([(
                    "id".into(),
                    Value::from("one"),
                )]))],
            );
        write.call_method("abort", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(abort_count.get(), 1);

        let read = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let all = read
            .call_method("objectStore", vec![Value::from("items")])
            .call_method("getAll", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(all.get_property("result").iter().count(), 0);
    }

    #[test]
    fn aborting_a_queued_transaction_removes_it_from_the_schedule() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("queued-abort"));
        let factory = factory_value();
        let open = factory.call_method(
            "open",
            vec![Value::from("queued-abort"), Value::Number(1.0)],
        );
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");

        let first = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        first
            .call_method("objectStore", vec![Value::from("items")])
            .call_method(
                "put",
                vec![Value::object(HashMap::from([(
                    "id".into(),
                    Value::from("first"),
                )]))],
            );

        let cancelled = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        cancelled.call_method("abort", vec![]);

        let read = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let all = read
            .call_method("objectStore", vec![Value::from("items")])
            .call_method("getAll", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(all.get_property("result").iter().count(), 1);
    }

    #[test]
    fn preventing_a_request_error_keeps_the_transaction_alive() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("prevent-request-error"));
        let factory = factory_value();
        let open = factory.call_method(
            "open",
            vec![Value::from("prevent-request-error"), Value::Number(1.0)],
        );
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");
        let transaction = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let complete_count = Rc::new(Cell::new(0));
        let observed_complete = complete_count.clone();
        transaction.set_property(
            "oncomplete",
            func(move |_, _| {
                observed_complete.set(observed_complete.get() + 1);
                Value::Undefined
            }),
        );
        let request = transaction
            .call_method("objectStore", vec![Value::from("items")])
            .call_method(
                "put",
                vec![Value::object(HashMap::from([(
                    "id".into(),
                    Value::from("not-written"),
                )]))],
            );
        request.set_property(
            "onerror",
            func(move |_, args| {
                arg(&args, 0).call_method("preventDefault", vec![]);
                Value::Undefined
            }),
        );

        crate::jsdom::drain_microtasks();
        assert_eq!(
            request
                .get_property("error")
                .get_property("name")
                .to_js_string(),
            "ReadOnlyError"
        );
        assert_eq!(complete_count.get(), 1);
    }

    #[test]
    fn upgrade_waits_for_existing_connection_to_close() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("connection-upgrade"));
        let factory = factory_value();
        let initial = factory.call_method(
            "open",
            vec![Value::from("connection-upgrade"), Value::Number(1.0)],
        );
        crate::jsdom::drain_microtasks();
        let connection = initial.get_property("result");

        let versionchange_count = Rc::new(Cell::new(0));
        let observed_versionchange = versionchange_count.clone();
        connection.set_property(
            "onversionchange",
            func(move |_, args| {
                let event = arg(&args, 0);
                assert_eq!(event.get_property("oldVersion").to_u32(), 1);
                assert_eq!(event.get_property("newVersion").to_u32(), 2);
                observed_versionchange.set(observed_versionchange.get() + 1);
                Value::Undefined
            }),
        );

        let upgrade = factory.call_method(
            "open",
            vec![Value::from("connection-upgrade"), Value::Number(2.0)],
        );
        let blocked_count = Rc::new(Cell::new(0));
        let observed_blocked = blocked_count.clone();
        upgrade.set_property(
            "onblocked",
            func(move |_, args| {
                let event = arg(&args, 0);
                assert_eq!(event.get_property("oldVersion").to_u32(), 1);
                assert_eq!(event.get_property("newVersion").to_u32(), 2);
                observed_blocked.set(observed_blocked.get() + 1);
                Value::Undefined
            }),
        );

        crate::jsdom::drain_microtasks();
        assert_eq!(versionchange_count.get(), 1);
        assert_eq!(blocked_count.get(), 1);
        assert_eq!(upgrade.get_property("readyState").to_js_string(), "pending");

        connection.call_method("close", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(upgrade.get_property("readyState").to_js_string(), "done");
        assert_eq!(
            upgrade
                .get_property("result")
                .get_property("version")
                .to_u32(),
            2
        );
        assert_eq!(blocked_count.get(), 1);
    }

    #[test]
    fn delete_waits_for_existing_connection_to_close() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("connection-delete"));
        let factory = factory_value();
        let initial = factory.call_method(
            "open",
            vec![Value::from("connection-delete"), Value::Number(1.0)],
        );
        crate::jsdom::drain_microtasks();
        let connection = initial.get_property("result");

        let versionchange_count = Rc::new(Cell::new(0));
        let observed_versionchange = versionchange_count.clone();
        connection.set_property(
            "onversionchange",
            func(move |_, args| {
                let event = arg(&args, 0);
                assert_eq!(event.get_property("oldVersion").to_u32(), 1);
                assert_eq!(event.get_property("newVersion"), Value::Null);
                observed_versionchange.set(observed_versionchange.get() + 1);
                Value::Undefined
            }),
        );

        let deletion =
            factory.call_method("deleteDatabase", vec![Value::from("connection-delete")]);
        let blocked_count = Rc::new(Cell::new(0));
        let observed_blocked = blocked_count.clone();
        deletion.set_property(
            "onblocked",
            func(move |_, args| {
                assert_eq!(arg(&args, 0).get_property("newVersion"), Value::Null);
                observed_blocked.set(observed_blocked.get() + 1);
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(versionchange_count.get(), 1);
        assert_eq!(blocked_count.get(), 1);
        assert_eq!(
            deletion.get_property("readyState").to_js_string(),
            "pending"
        );

        connection.call_method("close", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(deletion.get_property("readyState").to_js_string(), "done");
        assert_eq!(indexed_db::current_version("connection-delete").unwrap(), 0);
    }

    #[test]
    fn request_errors_reach_transaction_and_database_before_abort() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("error-bubbling"));
        let factory = factory_value();
        let open = factory.call_method(
            "open",
            vec![Value::from("error-bubbling"), Value::Number(1.0)],
        );
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");
        let database_errors = Rc::new(Cell::new(0));
        let observed_database_errors = database_errors.clone();
        database.set_property(
            "onerror",
            func(move |_, _| {
                observed_database_errors.set(observed_database_errors.get() + 1);
                Value::Undefined
            }),
        );

        let transaction = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let transaction_errors = Rc::new(Cell::new(0));
        let observed_transaction_errors = transaction_errors.clone();
        transaction.set_property(
            "onerror",
            func(move |_, _| {
                observed_transaction_errors.set(observed_transaction_errors.get() + 1);
                Value::Undefined
            }),
        );
        let aborts = Rc::new(Cell::new(0));
        let observed_aborts = aborts.clone();
        transaction.set_property(
            "onabort",
            func(move |_, _| {
                observed_aborts.set(observed_aborts.get() + 1);
                Value::Undefined
            }),
        );
        transaction
            .call_method("objectStore", vec![Value::from("items")])
            .call_method(
                "put",
                vec![Value::object(HashMap::from([(
                    "id".into(),
                    Value::from("rejected"),
                )]))],
            );

        crate::jsdom::drain_microtasks();
        assert_eq!(transaction_errors.get(), 1);
        assert_eq!(database_errors.get(), 1);
        assert_eq!(aborts.get(), 1);
        assert_eq!(
            transaction
                .get_property("error")
                .get_property("name")
                .to_js_string(),
            "ReadOnlyError"
        );
    }

    #[test]
    fn key_ranges_filter_count_limit_and_delete() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("key-ranges"));
        let factory = factory_value();
        let open = factory.call_method("open", vec![Value::from("key-ranges"), Value::Number(1.0)]);
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");
        let write = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let store = write.call_method("objectStore", vec![Value::from("items")]);
        for id in 1..=5 {
            store.call_method(
                "put",
                vec![Value::object(HashMap::from([(
                    "id".into(),
                    Value::Number(id as f64),
                )]))],
            );
        }
        crate::jsdom::drain_microtasks();

        let ranges = key_range_constructor_value();
        let range = ranges.call_method(
            "bound",
            vec![
                Value::Number(2.0),
                Value::Number(4.0),
                Value::Bool(true),
                Value::Bool(false),
            ],
        );
        let read = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let store = read.call_method("objectStore", vec![Value::from("items")]);
        let rows = store.call_method("getAll", vec![range.clone(), Value::Number(1.0)]);
        let keys = store.call_method("getAllKeys", vec![range.clone()]);
        let count = store.call_method("count", vec![range]);
        crate::jsdom::drain_microtasks();
        assert_eq!(rows.get_property("result").iter().count(), 1);
        assert_eq!(keys.get_property("result").iter().count(), 2);
        assert_eq!(count.get_property("result").to_u32(), 2);

        let cleanup = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let upper = ranges.call_method("upperBound", vec![Value::Number(2.0), Value::Bool(false)]);
        cleanup
            .call_method("objectStore", vec![Value::from("items")])
            .call_method("delete", vec![upper]);
        crate::jsdom::drain_microtasks();

        let verify = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let remaining = verify
            .call_method("objectStore", vec![Value::from("items")])
            .call_method("count", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(remaining.get_property("result").to_u32(), 3);
    }

    #[test]
    fn cursor_reuses_its_request_across_continue_and_advance() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("cursor"));
        let factory = factory_value();
        let open = factory.call_method("open", vec![Value::from("cursor"), Value::Number(1.0)]);
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");
        let write = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let store = write.call_method("objectStore", vec![Value::from("items")]);
        for id in 1..=5 {
            store.call_method(
                "put",
                vec![Value::object(HashMap::from([(
                    "id".into(),
                    Value::Number(id as f64),
                )]))],
            );
        }
        crate::jsdom::drain_microtasks();

        let read = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let request = read
            .call_method("objectStore", vec![Value::from("items")])
            .call_method("openCursor", vec![Value::Undefined, Value::from("next")]);
        let keys = Rc::new(RefCell::new(Vec::<u32>::new()));
        let observed_keys = keys.clone();
        let observed_request = request.clone();
        request.set_property(
            "onsuccess",
            func(move |_, _| {
                let cursor = observed_request.get_property("result");
                if cursor == Value::Null {
                    return Value::Undefined;
                }
                observed_keys
                    .borrow_mut()
                    .push(cursor.get_property("key").to_u32());
                if observed_keys.borrow().len() == 1 {
                    cursor.call_method("advance", vec![Value::Number(2.0)]);
                } else if cursor.get_property("key").to_u32() == 3 {
                    cursor.call_method("continue", vec![Value::Number(5.0)]);
                } else {
                    cursor.call_method("continue", vec![]);
                }
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*keys.borrow(), &[1, 3, 5]);
        assert_eq!(request.get_property("result"), Value::Null);

        let reverse = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let first = reverse
            .call_method("objectStore", vec![Value::from("items")])
            .call_method("openKeyCursor", vec![Value::Undefined, Value::from("prev")]);
        crate::jsdom::drain_microtasks();
        let cursor = first.get_property("result");
        assert_eq!(cursor.get_property("key").to_u32(), 5);
        assert_eq!(cursor.get_property("value"), Value::Undefined);
    }

    #[test]
    fn indexes_support_options_queries_and_unique_cursors() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("index-api"));
        let factory = factory_value();
        let open = factory.call_method("open", vec![Value::from("index-api"), Value::Number(1.0)]);
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                let store = upgrade_open.get_property("result").call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([("keyPath".into(), Value::from("id"))])),
                    ],
                );
                store.call_method(
                    "createIndex",
                    vec![
                        Value::from("by_email"),
                        Value::from("email"),
                        Value::object(HashMap::from([("unique".into(), Value::Bool(true))])),
                    ],
                );
                store.call_method(
                    "createIndex",
                    vec![
                        Value::from("by_tag"),
                        Value::from("tags"),
                        Value::object(HashMap::from([("multiEntry".into(), Value::Bool(true))])),
                    ],
                );
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");
        let write = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let store = write.call_method("objectStore", vec![Value::from("items")]);
        store.call_method(
            "put",
            vec![Value::object(HashMap::from([
                ("id".into(), Value::Number(1.0)),
                ("email".into(), Value::from("one@example.test")),
                (
                    "tags".into(),
                    Value::array(vec![Value::from("red"), Value::from("blue")]),
                ),
            ]))],
        );
        store.call_method(
            "put",
            vec![Value::object(HashMap::from([
                ("id".into(), Value::Number(2.0)),
                ("email".into(), Value::from("two@example.test")),
                (
                    "tags".into(),
                    Value::array(vec![Value::from("red"), Value::from("red")]),
                ),
            ]))],
        );
        crate::jsdom::drain_microtasks();

        let read = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let store = read.call_method("objectStore", vec![Value::from("items")]);
        let email = store.call_method("index", vec![Value::from("by_email")]);
        let tags = store.call_method("index", vec![Value::from("by_tag")]);
        assert_eq!(email.get_property("keyPath").to_js_string(), "email");
        assert_eq!(email.get_property("unique"), Value::Bool(true));
        assert_eq!(tags.get_property("multiEntry"), Value::Bool(true));
        let get = email.call_method("get", vec![Value::from("one@example.test")]);
        let get_key = email.call_method("getKey", vec![Value::from("two@example.test")]);
        let red = tags.call_method("getAll", vec![Value::from("red")]);
        let red_keys = tags.call_method("getAllKeys", vec![Value::from("red")]);
        let red_count = tags.call_method("count", vec![Value::from("red")]);

        let cursor_request = tags.call_method(
            "openKeyCursor",
            vec![Value::Undefined, Value::from("nextunique")],
        );
        let cursor_keys = Rc::new(RefCell::new(Vec::<String>::new()));
        let observed_keys = cursor_keys.clone();
        let observed_request = cursor_request.clone();
        cursor_request.set_property(
            "onsuccess",
            func(move |_, _| {
                let cursor = observed_request.get_property("result");
                if cursor != Value::Null {
                    observed_keys
                        .borrow_mut()
                        .push(cursor.get_property("key").to_js_string());
                    cursor.call_method("continue", vec![]);
                }
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(get.get_property("result").get_property("id").to_u32(), 1);
        assert_eq!(get_key.get_property("result").to_u32(), 2);
        assert_eq!(red.get_property("result").iter().count(), 2);
        assert_eq!(red_keys.get_property("result").iter().count(), 2);
        assert_eq!(red_count.get_property("result").to_u32(), 2);
        assert_eq!(&*cursor_keys.borrow(), &["blue", "red"]);

        let duplicate = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let request = duplicate
            .call_method("objectStore", vec![Value::from("items")])
            .call_method(
                "put",
                vec![Value::object(HashMap::from([
                    ("id".into(), Value::Number(3.0)),
                    ("email".into(), Value::from("one@example.test")),
                ]))],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(
            request
                .get_property("error")
                .get_property("name")
                .to_js_string(),
            "ConstraintError"
        );
    }

    #[test]
    fn compound_paths_dates_clone_values_and_schema_mutation_follow_the_web_surface() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("web-completeness"));
        let factory = factory_value();
        let open = factory.call_method(
            "open",
            vec![Value::from("web-completeness"), Value::Number(1.0)],
        );
        let upgrade_open = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                let database = upgrade_open.get_property("result");
                let store = database.call_method(
                    "createObjectStore",
                    vec![
                        Value::from("items"),
                        Value::object(HashMap::from([(
                            "keyPath".into(),
                            Value::array(vec![Value::from("org"), Value::from("id")]),
                        )])),
                    ],
                );
                store.call_method(
                    "createIndex",
                    vec![Value::from("by_when"), Value::from("when")],
                );
                store.call_method(
                    "createIndex",
                    vec![Value::from("by_binary"), Value::from("binary")],
                );
                database.call_method("createObjectStore", vec![Value::from("temporary")]);
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let database = open.get_property("result");
        let write = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readwrite")],
        );
        let store = write.call_method("objectStore", vec![Value::from("items")]);
        assert_eq!(store.get_property("keyPath").iter().count(), 2);
        let when = w3cos_core::web::date_value(1_700_000_000_000.0);
        let binary = w3cos_core::collections::typed_array_value(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(255.0),
        ]);
        let metadata = Value::object(HashMap::from([("label".into(), Value::from("cycle"))]));
        metadata.set_property("self", metadata.clone());
        store.call_method(
            "put",
            vec![Value::object(HashMap::from([
                ("org".into(), Value::from("acme")),
                ("id".into(), Value::Number(7.0)),
                ("when".into(), when.clone()),
                ("binary".into(), binary.clone()),
                ("metadata".into(), metadata),
                ("optional".into(), Value::Undefined),
                ("score".into(), Value::Number(f64::NAN)),
            ]))],
        );
        crate::jsdom::drain_microtasks();

        let read = database.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        let store = read.call_method("objectStore", vec![Value::from("items")]);
        let row = store.call_method(
            "get",
            vec![Value::array(vec![Value::from("acme"), Value::Number(7.0)])],
        );
        let indexed = store
            .call_method("index", vec![Value::from("by_when")])
            .call_method("get", vec![when.clone()]);
        let binary_indexed = store
            .call_method("index", vec![Value::from("by_binary")])
            .call_method("get", vec![binary.clone()]);
        crate::jsdom::drain_microtasks();
        let value = row.get_property("result");
        assert_eq!(value.get_property("optional"), Value::Undefined);
        assert!(value.get_property("score").to_number().is_nan());
        let restored_metadata = value.get_property("metadata");
        assert!(
            restored_metadata
                .get_property("self")
                .strict_eq(&restored_metadata)
        );
        assert_eq!(
            value
                .get_property("when")
                .call_method("getTime", vec![])
                .to_number(),
            1_700_000_000_000.0
        );
        assert_eq!(
            indexed.get_property("result").get_property("id").to_u32(),
            7
        );
        assert_eq!(
            binary_indexed
                .get_property("result")
                .get_property("binary")
                .iter()
                .map(|value| value.to_u32())
                .collect::<Vec<_>>(),
            vec![1, 2, 255]
        );
        assert_eq!(
            factory
                .call_method(
                    "cmp",
                    vec![Value::Number(1.0), w3cos_core::web::date_value(0.0)]
                )
                .to_i32(),
            -1
        );

        let listed = Rc::new(RefCell::new(Vec::<String>::new()));
        let observed = listed.clone();
        factory.call_method("databases", vec![]).call_method(
            "then",
            vec![func(move |_, args| {
                *observed.borrow_mut() = arg(&args, 0)
                    .iter()
                    .map(|entry| entry.get_property("name").to_js_string())
                    .collect();
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*listed.borrow(), &["web-completeness"]);

        database.call_method("close", vec![]);
        let upgrade = factory.call_method(
            "open",
            vec![Value::from("web-completeness"), Value::Number(2.0)],
        );
        let upgrade_request = upgrade.clone();
        upgrade.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                let transaction = upgrade_request.get_property("transaction");
                transaction
                    .call_method("objectStore", vec![Value::from("items")])
                    .call_method("deleteIndex", vec![Value::from("by_when")]);
                transaction
                    .call_method("objectStore", vec![Value::from("items")])
                    .call_method("deleteIndex", vec![Value::from("by_binary")]);
                upgrade_request
                    .get_property("result")
                    .call_method("deleteObjectStore", vec![Value::from("temporary")]);
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        let upgraded = upgrade.get_property("result");
        assert_eq!(upgraded.get_property("objectStoreNames").iter().count(), 1);
        let transaction = upgraded.call_method(
            "transaction",
            vec![Value::from("items"), Value::from("readonly")],
        );
        assert_eq!(
            transaction
                .call_method("objectStore", vec![Value::from("items")])
                .get_property("indexNames")
                .iter()
                .count(),
            0
        );
    }

    #[test]
    fn aborting_the_initial_versionchange_leaves_no_partial_schema() {
        let _guard = indexed_db::IDB_TEST_LOCK.lock().unwrap();
        indexed_db::set_base_dir(fresh_dir("initial-upgrade-abort"));
        let factory = factory_value();
        let open = factory.call_method(
            "open",
            vec![Value::from("initial-upgrade-abort"), Value::Number(1.0)],
        );
        let upgrade = open.clone();
        open.set_property(
            "onupgradeneeded",
            func(move |_, _| {
                upgrade
                    .get_property("result")
                    .call_method("createObjectStore", vec![Value::from("must-not-persist")]);
                upgrade
                    .get_property("transaction")
                    .call_method("abort", vec![]);
                Value::Undefined
            }),
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(
            open.get_property("error")
                .get_property("name")
                .to_js_string(),
            "AbortError"
        );
        assert_eq!(
            indexed_db::current_version("initial-upgrade-abort").unwrap(),
            0
        );
    }

    #[test]
    fn pinned_wpt_subset_has_no_silent_skips() {
        let manifest: JsonValue =
            serde_json::from_str(include_str!("../../../tests/wpt/indexeddb-subset.json")).unwrap();
        let revision = manifest["revision"].as_str().unwrap();
        assert_eq!(revision.len(), 40, "WPT revision must be a full SHA");
        assert_eq!(
            manifest["mode"],
            JsonValue::String("adapted-assertions".into())
        );
        let source = include_str!("indexed_db_web.rs");
        let tests = manifest["tests"].as_array().unwrap();
        assert!(!tests.is_empty());
        for test in tests {
            assert_eq!(test["status"], JsonValue::String("covered".into()));
            assert!(test["path"].as_str().unwrap().starts_with("IndexedDB/"));
            let rust_test = test["rust_test"].as_str().unwrap();
            assert!(
                source.contains(&format!("fn {rust_test}()")),
                "missing adapted assertion test {rust_test}"
            );
        }
    }
}
