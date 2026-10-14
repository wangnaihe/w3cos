//! `navigator.w3cos` — namespaced, permission-gated capabilities that have
//! no W3C or WICG draft.
//!
//! The namespace is deliberately not a root `Navigator` member. See
//! `docs/W3COS_EXTENSIONS.md`. Process spawn and global shortcuts run against
//! the host. Every other surface in this namespace rejects with
//! `NotSupportedError` after its permission check.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::jsdom::realm_function;
use w3cos_core::Value;

/// Same capability bits as `w3cos_ai_bridge::permissions::AgentPermissions`.
/// Hosts must not grant the page name without the matching bridge field.
pub const PERMISSION_PROCESS: &str = "w3cos-process";
pub const PERMISSION_SHORTCUTS: &str = "w3cos-global-shortcuts";
pub const PERMISSION_TRAY: &str = "w3cos-tray";
pub const PERMISSION_WINDOWS: &str = "w3cos-window-manager";
pub const PERMISSION_DAEMON: &str = "w3cos-daemon";
pub const PERMISSION_SYSTEM: &str = "w3cos-system";
pub const PERMISSION_KEYRING: &str = "w3cos-keyring";
pub const PERMISSION_ACCESSIBILITY: &str = "w3cos-accessibility";
pub const PERMISSION_CAPTURE: &str = "display-capture";

struct Shortcut {
    id: u64,
    accelerator: String,
    callback: Value,
    global: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
    key: String,
}

thread_local! {
    static SHORTCUTS: RefCell<Vec<Shortcut>> = const { RefCell::new(Vec::new()) };
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
thread_local! {
    static PROCESSES: RefCell<HashMap<u64, crate::process::ChildProcess>> =
        RefCell::new(HashMap::new());
}

static SHORTCUTS_ACTIVE: AtomicBool = AtomicBool::new(false);
static NEXT_SHORTCUT: AtomicU64 = AtomicU64::new(1);
static NEXT_PROCESS: AtomicU64 = AtomicU64::new(1);

fn error(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn rejected(name: &str, message: &str) -> Value {
    w3cos_core::promise::reject(vec![error(name, message)])
}

fn refuse(permission: &str, feature: &str) -> Value {
    if let Some(rejected) = crate::capability_gate::reject_unless_granted(permission) {
        return rejected;
    }
    rejected(
        "NotSupportedError",
        &format!("{feature} has no W3C draft and no platform adapter; the call was refused"),
    )
}

fn refresh_active_flag() {
    let active = SHORTCUTS.with(|shortcuts| !shortcuts.borrow().is_empty());
    SHORTCUTS_ACTIVE.store(active, Ordering::Relaxed);
}

pub fn shortcuts_active() -> bool {
    SHORTCUTS_ACTIVE.load(Ordering::Relaxed)
}

fn parse_accelerator(accelerator: &str) -> Result<(bool, bool, bool, bool, String), String> {
    let parts = accelerator
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some(key) = parts.last() else {
        return Err("accelerator is empty".into());
    };
    if key.chars().count() != 1 {
        return Err(format!("unsupported key {key}"));
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut meta = false;
    for part in &parts[..parts.len() - 1] {
        match part.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            "meta" | "super" | "command" => meta = true,
            other => return Err(format!("unknown modifier {other}")),
        }
    }
    Ok((ctrl, alt, shift, meta, key.to_ascii_lowercase()))
}

fn sync_grabs(previous: &[String]) -> Result<(), String> {
    let next = SHORTCUTS.with(|shortcuts| {
        shortcuts
            .borrow()
            .iter()
            .filter(|shortcut| shortcut.global)
            .map(|shortcut| shortcut.accelerator.clone())
            .collect::<Vec<_>>()
    });
    if let Err(err) = crate::platform_host::install_grabs(&next) {
        let _ = crate::platform_host::install_grabs(previous);
        return Err(err);
    }
    Ok(())
}

pub fn dispatch_accelerator(accelerator: &str) -> bool {
    let Ok((_, _, _, _, key)) = parse_accelerator(accelerator) else {
        return false;
    };
    let (ctrl, alt, shift, meta) = parse_accelerator(accelerator)
        .map(|(ctrl, alt, shift, meta, _)| (ctrl, alt, shift, meta))
        .unwrap_or((false, false, false, false));
    dispatch_matching_shortcut(&key, ctrl, alt, shift, meta)
}

pub fn dispatch_matching_shortcut(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> bool {
    if !shortcuts_active() {
        return false;
    }
    let key = key.to_ascii_lowercase();
    let callbacks = SHORTCUTS.with(|shortcuts| {
        shortcuts
            .borrow()
            .iter()
            .filter(|shortcut| {
                shortcut.key == key
                    && shortcut.ctrl == ctrl
                    && shortcut.alt == alt
                    && shortcut.shift == shift
                    && shortcut.meta == meta
            })
            .map(|shortcut| shortcut.callback.clone())
            .collect::<Vec<_>>()
    });
    if callbacks.is_empty() {
        return false;
    }
    for callback in callbacks {
        if callback.is_function() {
            callback.call(Value::Undefined, vec![Value::string(&key)]);
        }
    }
    true
}

pub fn flush_global_shortcuts() -> usize {
    let pending = crate::platform_host::poll_grabs();
    let count = pending.len();
    for accelerator in pending {
        dispatch_accelerator(&accelerator);
    }
    count
}

fn string_list(value: &Value) -> Vec<String> {
    let length = value.get_property("length").to_u32() as usize;
    (0..length)
        .map(|index| value.get_property(&index.to_string()).to_js_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn blob_from_png(bytes: &[u8]) -> Value {
    let buffer = w3cos_core::binary::array_buffer_value(bytes.to_vec());
    let view = w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Uint8Array"),
        vec![buffer],
    );
    w3cos_core::class::construct(
        &crate::files::blob_class(),
        vec![
            Value::array(vec![view]),
            Value::object(HashMap::from([("type".into(), Value::string("image/png"))])),
        ],
    )
}

fn scope_is_foreign(options: &Value) -> bool {
    let scope = options.get_property("scope").to_js_string();
    matches!(scope.as_str(), "system" | "cross-app" | "background")
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn spawn_process(options: &Value) -> Result<Value, String> {
    let file = options.get_property("file").to_js_string();
    if file.is_empty() || file.contains('\0') {
        return Err("process.spawn requires a file path".into());
    }
    let args = string_list(&options.get_property("args"));
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut spawn_options = crate::process::SpawnOptions {
        pipe_stdin: true,
        pipe_stdout: true,
        pipe_stderr: true,
        ..crate::process::SpawnOptions::default()
    };
    let cwd = options.get_property("cwd").to_js_string();
    if !cwd.is_empty() {
        spawn_options.cwd = Some(cwd);
    }
    let child = crate::process::spawn(&file, &arg_refs, spawn_options)?;
    let pid = child.id();
    let id = NEXT_PROCESS.fetch_add(1, Ordering::Relaxed);
    PROCESSES.with(|processes| {
        processes.borrow_mut().insert(id, child);
    });
    Ok(process_value(id, pid))
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn process_value(id: u64, pid: u32) -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::from([
        ("pid".into(), Value::Number(f64::from(pid))),
        ("id".into(), Value::Number(id as f64)),
    ]));
    value.set_property(
        "writeStdin",
        realm_function(generation, move |_, args| {
            let data = args.first().map(Value::to_js_string).unwrap_or_default();
            let wrote = PROCESSES.with(|processes| {
                processes
                    .borrow_mut()
                    .get_mut(&id)
                    .map(|child| child.write_stdin(&data))
            });
            match wrote {
                Some(Ok(())) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Some(Err(message)) => rejected("NetworkError", &message),
                None => rejected("InvalidStateError", "process is no longer running"),
            }
        }),
    );
    value.set_property(
        "closeStdin",
        realm_function(generation, move |_, _| {
            PROCESSES.with(|processes| {
                if let Some(child) = processes.borrow_mut().get_mut(&id) {
                    child.close_stdin();
                }
            });
            Value::Undefined
        }),
    );
    value.set_property(
        "kill",
        realm_function(generation, move |_, _| {
            let killed = PROCESSES.with(|processes| {
                processes
                    .borrow_mut()
                    .get_mut(&id)
                    .map(|child| child.kill())
            });
            match killed {
                Some(Ok(())) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Some(Err(message)) => rejected("InvalidStateError", &message),
                None => rejected("InvalidStateError", "process is no longer running"),
            }
        }),
    );
    value.set_property(
        "wait",
        realm_function(generation, move |_, _| {
            let child = PROCESSES.with(|processes| processes.borrow_mut().remove(&id));
            let Some(child) = child else {
                return rejected("InvalidStateError", "process is no longer running");
            };
            let result = child.wait_with_output();
            w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                ("stdout".into(), Value::string(&result.stdout)),
                ("stderr".into(), Value::string(&result.stderr)),
                (
                    "exitCode".into(),
                    Value::Number(f64::from(result.exit_code)),
                ),
                ("ok".into(), Value::Bool(result.ok)),
            ]))])
        }),
    );
    value
}

#[cfg(not(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn spawn_process(_options: &Value) -> Result<Value, String> {
    Err("process spawn has no platform adapter".into())
}

fn process_namespace() -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    value.set_property(
        "spawn",
        realm_function(generation, |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_PROCESS)
            {
                return rejected;
            }
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            match spawn_process(&options) {
                Ok(process) => w3cos_core::promise::resolve(vec![process]),
                Err(message) => rejected("NotSupportedError", &message),
            }
        }),
    );
    value.set_property(
        "exec",
        realm_function(generation, |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_PROCESS)
            {
                return rejected;
            }
            #[cfg(all(
                any(target_os = "macos", target_os = "linux", target_os = "windows"),
                not(target_env = "ohos")
            ))]
            {
                let options = args.first().cloned().unwrap_or(Value::Undefined);
                let file = options.get_property("file").to_js_string();
                let args = string_list(&options.get_property("args"));
                let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                if file.is_empty() {
                    return rejected("TypeError", "process.exec requires a file path");
                }
                let result = crate::process::exec(&file, &arg_refs);
                return w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                    ("stdout".into(), Value::string(&result.stdout)),
                    ("stderr".into(), Value::string(&result.stderr)),
                    (
                        "exitCode".into(),
                        Value::Number(f64::from(result.exit_code)),
                    ),
                    ("ok".into(), Value::Bool(result.ok)),
                ]))]);
            }
            #[cfg(not(all(
                any(target_os = "macos", target_os = "linux", target_os = "windows"),
                not(target_env = "ohos")
            )))]
            {
                let _ = args;
                rejected("NotSupportedError", "process exec has no platform adapter")
            }
        }),
    );
    value
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn ipc_namespace() -> Value {
    use std::cell::Cell;
    thread_local! {
        static SERVERS: RefCell<HashMap<u64, crate::ipc::IpcServer>> = RefCell::new(HashMap::new());
        static CLIENTS: RefCell<HashMap<u64, crate::ipc::IpcClient>> = RefCell::new(HashMap::new());
        static NEXT: Cell<u64> = const { Cell::new(1) };
    }
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    value.set_property(
        "listen",
        realm_function(generation, move |_, _| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_PROCESS)
            {
                return rejected;
            }
            let endpoint = std::env::temp_dir().join(format!(
                "w3cos-ipc-{}-{}.sock",
                std::process::id(),
                NEXT.with(|next| {
                    let id = next.get();
                    next.set(id + 1);
                    id
                })
            ));
            let endpoint = endpoint.to_string_lossy().to_string();
            match crate::ipc::IpcServer::bind(&endpoint) {
                Ok(server) => {
                    let id = NEXT.with(|next| {
                        let id = next.get();
                        next.set(id + 1);
                        id
                    });
                    SERVERS.with(|servers| {
                        servers.borrow_mut().insert(id, server);
                    });
                    let handle = Value::object(HashMap::from([
                        ("endpoint".into(), Value::string(&endpoint)),
                        ("id".into(), Value::Number(id as f64)),
                    ]));
                    handle.set_property(
                        "receive",
                        realm_function(generation, move |_, _| {
                            let started = std::time::Instant::now();
                            loop {
                                let message = SERVERS.with(|servers| {
                                    servers
                                        .borrow()
                                        .get(&id)
                                        .and_then(|server| server.try_recv())
                                });
                                if let Some((_peer, message)) = message {
                                    return w3cos_core::promise::resolve(vec![Value::object(
                                        HashMap::from([
                                            ("channel".into(), Value::string(&message.channel)),
                                            (
                                                "payload".into(),
                                                Value::string(
                                                    &message.payload.as_str().unwrap_or(""),
                                                ),
                                            ),
                                        ]),
                                    )]);
                                }
                                if started.elapsed() > std::time::Duration::from_millis(500) {
                                    return w3cos_core::promise::resolve(vec![Value::Null]);
                                }
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                        }),
                    );
                    w3cos_core::promise::resolve(vec![handle])
                }
                Err(err) => rejected("NetworkError", &err.to_string()),
            }
        }),
    );
    value.set_property(
        "connect",
        realm_function(generation, move |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_PROCESS)
            {
                return rejected;
            }
            let endpoint = args.first().map(Value::to_js_string).unwrap_or_default();
            match crate::ipc::IpcClient::connect(&endpoint) {
                Ok(client) => {
                    let id = NEXT.with(|next| {
                        let id = next.get();
                        next.set(id + 1);
                        id
                    });
                    CLIENTS.with(|clients| {
                        clients.borrow_mut().insert(id, client);
                    });
                    let port = Value::object(HashMap::from([(
                        "endpoint".into(),
                        Value::string(&endpoint),
                    )]));
                    port.set_property(
                        "postMessage",
                        realm_function(generation, move |_, args| {
                            let channel = args.first().map(Value::to_js_string).unwrap_or_default();
                            let payload = args.get(1).map(Value::to_js_string).unwrap_or_default();
                            let sent = CLIENTS.with(|clients| {
                                clients.borrow().get(&id).map(|client| {
                                    client.send(channel, serde_json::Value::String(payload))
                                })
                            });
                            match sent {
                                Some(Ok(())) => {
                                    w3cos_core::promise::resolve(vec![Value::Undefined])
                                }
                                Some(Err(message)) => rejected("NetworkError", &message),
                                None => rejected("InvalidStateError", "IPC port is closed"),
                            }
                        }),
                    );
                    w3cos_core::promise::resolve(vec![port])
                }
                Err(err) => rejected("NetworkError", &err.to_string()),
            }
        }),
    );
    value
}

#[cfg(not(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn ipc_namespace() -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    for method in ["listen", "connect"] {
        value.set_property(
            method,
            realm_function(generation, |_, _| {
                refuse(PERMISSION_PROCESS, "navigator.w3cos.ipc")
            }),
        );
    }
    value
}

fn shortcuts_namespace() -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    value.set_property(
        "register",
        realm_function(generation, move |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_SHORTCUTS)
            {
                return rejected;
            }
            let accelerator = args.first().map(Value::to_js_string).unwrap_or_default();
            let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
            let options = args.get(2).cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                return rejected("TypeError", "shortcuts.register requires a callback");
            }
            let (ctrl, alt, shift, meta, key) = match parse_accelerator(&accelerator) {
                Ok(parsed) => parsed,
                Err(message) => return rejected("TypeError", &message),
            };
            let global = if options.get_property("global").is_undefined() {
                true
            } else {
                options.get_property("global").to_bool()
            };
            let previous = SHORTCUTS.with(|shortcuts| {
                shortcuts
                    .borrow()
                    .iter()
                    .filter(|shortcut| shortcut.global)
                    .map(|shortcut| shortcut.accelerator.clone())
                    .collect::<Vec<_>>()
            });
            let id = NEXT_SHORTCUT.fetch_add(1, Ordering::Relaxed);
            SHORTCUTS.with(|shortcuts| {
                shortcuts.borrow_mut().push(Shortcut {
                    id,
                    accelerator: accelerator.clone(),
                    callback,
                    global,
                    ctrl,
                    alt,
                    shift,
                    meta,
                    key,
                });
            });
            refresh_active_flag();
            if global {
                if let Err(message) = sync_grabs(&previous) {
                    SHORTCUTS.with(|shortcuts| {
                        shortcuts.borrow_mut().retain(|shortcut| shortcut.id != id);
                    });
                    refresh_active_flag();
                    return rejected("NotSupportedError", &message);
                }
            }
            let handle = Value::object(HashMap::from([
                ("id".into(), Value::Number(id as f64)),
                ("accelerator".into(), Value::string(&accelerator)),
                (
                    "scope".into(),
                    Value::string(if global { "system" } else { "application" }),
                ),
            ]));
            handle.set_property(
                "unregister",
                realm_function(generation, move |_, _| {
                    let previous = SHORTCUTS.with(|shortcuts| {
                        shortcuts
                            .borrow()
                            .iter()
                            .filter(|shortcut| shortcut.global)
                            .map(|shortcut| shortcut.accelerator.clone())
                            .collect::<Vec<_>>()
                    });
                    SHORTCUTS.with(|shortcuts| {
                        shortcuts.borrow_mut().retain(|shortcut| shortcut.id != id);
                    });
                    refresh_active_flag();
                    let _ = sync_grabs(&previous);
                    Value::Undefined
                }),
            );
            w3cos_core::promise::resolve(vec![handle])
        }),
    );
    value
}

fn stub_namespace(
    permission: &'static str,
    feature: &'static str,
    methods: &'static [&'static str],
) -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    for method in methods {
        let feature = feature.to_string();
        value.set_property(
            method,
            realm_function(generation, move |_, _| refuse(permission, &feature)),
        );
    }
    value
}

fn agent_namespace() -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    value.set_property(
        "accessibilityTree",
        realm_function(generation, |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_ACCESSIBILITY)
            {
                return rejected;
            }
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            if scope_is_foreign(&options) {
                return rejected(
                    "NotSupportedError",
                    "cross-app accessibility trees are refused; this call only reads the current app, the same tree as AI Bridge",
                );
            }
            let json = crate::dom::with_document(|document| {
                let tree = w3cos_a11y::tree::build_a11y_tree(document);
                serde_json::to_string(&tree).unwrap_or_else(|_| "{}".into())
            });
            w3cos_core::promise::resolve(vec![Value::string(&json)])
        }),
    );
    value.set_property(
        "captureScreen",
        realm_function(generation, |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_CAPTURE)
            {
                return rejected;
            }
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            if scope_is_foreign(&options) {
                return rejected(
                    "NotSupportedError",
                    "background or cross-app screen capture is refused",
                );
            }
            match crate::platform_host::capture_display() {
                Ok(image) => w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                    ("width".into(), Value::Number(f64::from(image.width))),
                    ("height".into(), Value::Number(f64::from(image.height))),
                    ("blob".into(), blob_from_png(&image.png)),
                ]))]),
                Err(message) => rejected("NotSupportedError", &message),
            }
        }),
    );
    value
}

pub fn namespace_value() -> Value {
    Value::object(HashMap::from([
        ("process".into(), process_namespace()),
        ("ipc".into(), ipc_namespace()),
        ("shortcuts".into(), shortcuts_namespace()),
        (
            "tray".into(),
            stub_namespace(
                PERMISSION_TRAY,
                "navigator.w3cos.tray",
                &["show", "setMenu"],
            ),
        ),
        (
            "windows".into(),
            stub_namespace(
                PERMISSION_WINDOWS,
                "navigator.w3cos.windows",
                &["list", "focus", "setAlwaysOnTop"],
            ),
        ),
        (
            "daemon".into(),
            stub_namespace(
                PERMISSION_DAEMON,
                "navigator.w3cos.daemon",
                &["install", "start", "enableAutostart"],
            ),
        ),
        (
            "system".into(),
            stub_namespace(
                PERMISSION_SYSTEM,
                "navigator.w3cos.system",
                &["get", "set", "power"],
            ),
        ),
        (
            "keyring".into(),
            stub_namespace(
                PERMISSION_KEYRING,
                "navigator.w3cos.keyring",
                &["get", "set", "delete"],
            ),
        ),
        ("agent".into(), agent_namespace()),
    ]))
}

pub fn reset_realm() {
    SHORTCUTS.with(|shortcuts| shortcuts.borrow_mut().clear());
    #[cfg(all(
        any(target_os = "macos", target_os = "linux", target_os = "windows"),
        not(target_env = "ohos")
    ))]
    PROCESSES.with(|processes| processes.borrow_mut().clear());
    refresh_active_flag();
    crate::platform_host::clear_grabs();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn await_value(promise: Value) -> Result<Value, String> {
        let ok = Rc::new(RefCell::new(None));
        let err = Rc::new(RefCell::new(None));
        let ok_for_then = Rc::clone(&ok);
        let err_for_catch = Rc::clone(&err);
        promise.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *ok_for_then.borrow_mut() = args.first().cloned();
                Value::Undefined
            })],
        );
        promise.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *err_for_catch.borrow_mut() = args
                    .first()
                    .map(|value| value.get_property("name").to_js_string());
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        if let Some(name) = err.borrow().clone() {
            return Err(name);
        }
        ok.borrow().clone().ok_or_else(|| "unresolved".into())
    }

    #[test]
    fn process_spawn_and_ipc_require_permission_then_run() {
        reset_realm();
        crate::capability_gate::reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let namespace = namespace_value();
        let denied = await_value(namespace.get_property("process").call_method(
            "spawn",
            vec![Value::object(HashMap::from([(
                "file".into(),
                Value::string("/bin/echo"),
            )]))],
        ))
        .unwrap_err();
        assert_eq!(denied, "NotAllowedError");

        crate::capability_gate::grant(PERMISSION_PROCESS);
        let process = await_value(namespace.get_property("process").call_method(
            "exec",
            vec![Value::object(HashMap::from([
                ("file".into(), Value::string("/bin/echo")),
                ("args".into(), Value::array(vec![Value::string("w3cos")])),
            ]))],
        ))
        .unwrap();
        assert_eq!(process.get_property("stdout").to_js_string(), "w3cos\n");
        assert_eq!(process.get_property("exitCode").to_number(), 0.0);

        let server =
            await_value(namespace.get_property("ipc").call_method("listen", vec![])).unwrap();
        let endpoint = server.get_property("endpoint").to_js_string();
        let client = await_value(
            namespace
                .get_property("ipc")
                .call_method("connect", vec![Value::string(&endpoint)]),
        )
        .unwrap();
        await_value(client.call_method(
            "postMessage",
            vec![Value::string("ping"), Value::string("hello")],
        ))
        .unwrap();
        let message = await_value(server.call_method("receive", vec![])).unwrap();
        assert_eq!(message.get_property("channel").to_js_string(), "ping");
        assert_eq!(message.get_property("payload").to_js_string(), "hello");
        crate::capability_gate::reset();
        reset_realm();
    }

    #[test]
    fn shortcuts_deny_without_permission_and_dispatch_when_granted() {
        reset_realm();
        crate::capability_gate::reset();
        let namespace = namespace_value();
        let denied = await_value(namespace.get_property("shortcuts").call_method(
            "register",
            vec![
                Value::string("Control+Shift+K"),
                Value::function(|_, _| Value::Undefined),
            ],
        ))
        .unwrap_err();
        assert_eq!(denied, "NotAllowedError");

        crate::capability_gate::grant(PERMISSION_SHORTCUTS);
        let hits = Rc::new(RefCell::new(0));
        let hits_for_callback = Rc::clone(&hits);
        let handle = await_value(namespace.get_property("shortcuts").call_method(
            "register",
            vec![
                Value::string("Control+Shift+K"),
                Value::function(move |_, _| {
                    *hits_for_callback.borrow_mut() += 1;
                    Value::Undefined
                }),
                Value::object(HashMap::from([("global".into(), Value::Bool(false))])),
            ],
        ))
        .unwrap();
        assert_eq!(handle.get_property("scope").to_js_string(), "application");
        assert!(dispatch_accelerator("Control+Shift+K"));
        assert_eq!(*hits.borrow(), 1);
        assert!(!dispatch_matching_shortcut("k", false, false, false, false));
        handle.call_method("unregister", vec![]);
        assert!(!shortcuts_active());
        crate::capability_gate::reset();
        reset_realm();
    }

    #[test]
    fn stubs_refuse_after_permission_and_agent_tree_follows_the_bridge_bit() {
        reset_realm();
        crate::capability_gate::reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let namespace = namespace_value();
        let denied =
            await_value(namespace.get_property("tray").call_method("show", vec![])).unwrap_err();
        assert_eq!(denied, "NotAllowedError");
        crate::capability_gate::grant(PERMISSION_TRAY);
        crate::capability_gate::grant(PERMISSION_KEYRING);
        crate::capability_gate::grant(PERMISSION_SYSTEM);
        let refused = await_value(
            namespace
                .get_property("keyring")
                .call_method("set", vec![Value::string("token"), Value::string("secret")]),
        )
        .unwrap_err();
        assert_eq!(refused, "NotSupportedError");
        let power = await_value(
            namespace
                .get_property("system")
                .call_method("power", vec![Value::string("shutdown")]),
        )
        .unwrap_err();
        assert_eq!(power, "NotSupportedError");

        let button = crate::dom::create_element("button");
        crate::dom::set_text_content(button, "Submit");
        crate::dom::append_child(crate::dom::body_id(), button);
        let denied_tree = await_value(
            namespace
                .get_property("agent")
                .call_method("accessibilityTree", vec![]),
        )
        .unwrap_err();
        assert_eq!(denied_tree, "NotAllowedError");
        crate::capability_gate::grant(PERMISSION_ACCESSIBILITY);
        let tree = await_value(
            namespace
                .get_property("agent")
                .call_method("accessibilityTree", vec![]),
        )
        .unwrap();
        assert!(tree.to_js_string().contains("Submit"));
        let cross = await_value(namespace.get_property("agent").call_method(
            "accessibilityTree",
            vec![Value::object(HashMap::from([(
                "scope".into(),
                Value::string("cross-app"),
            )]))],
        ))
        .unwrap_err();
        assert_eq!(cross, "NotSupportedError");
        crate::capability_gate::reset();
        reset_realm();
    }
}
