//! `navigator.w3cos` — namespaced, permission-gated capabilities that have
//! no W3C or WICG draft.
//!
//! The namespace is deliberately not a root `Navigator` member. See
//! `docs/W3COS_EXTENSIONS.md`. Process spawn, global shortcuts, the system
//! tray, and the origin-scoped keyring run against the host. Every other
//! surface in this namespace rejects with `NotSupportedError` after its
//! permission check.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::jsdom::realm_function;
use crate::tray_host::{TrayEventKind, TrayMenuEntry, TraySpec};
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

const MAX_TRAY_TEXT: usize = 128;
const MAX_TRAY_ITEMS: usize = 40;

struct TrayFailure {
    name: &'static str,
    message: String,
}

struct LiveTray {
    spec: TraySpec,
    on_click: Option<Value>,
    on_menu: Option<Value>,
}

struct TrayRegistry {
    order: Vec<u64>,
    icons: HashMap<u64, LiveTray>,
}

thread_local! {
    static TRAYS: RefCell<TrayRegistry> = RefCell::new(TrayRegistry {
        order: Vec::new(),
        icons: HashMap::new(),
    });
    static TRAY_FLUSHING: Cell<bool> = const { Cell::new(false) };
    static TRAY_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

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

fn tray_failure(name: &'static str, message: impl Into<String>) -> TrayFailure {
    TrayFailure {
        name,
        message: message.into(),
    }
}

fn host_failure(message: String) -> TrayFailure {
    if message == "tray icon was removed" {
        tray_failure("InvalidStateError", message)
    } else {
        tray_failure("NotSupportedError", message)
    }
}

fn permit(body: impl FnOnce() -> Result<Value, TrayFailure>) -> Value {
    if let Some(rejected) = crate::capability_gate::reject_unless_granted(PERMISSION_TRAY) {
        return rejected;
    }
    match body() {
        Ok(value) => w3cos_core::promise::resolve(vec![value]),
        Err(failure) => rejected(failure.name, &failure.message),
    }
}

fn refresh_tray_flag() {
    let active = TRAYS.with(|trays| !trays.borrow().order.is_empty());
    TRAY_ACTIVE.with(|flag| flag.set(active));
}

fn empty_tray_spec() -> TraySpec {
    TraySpec {
        tooltip: String::new(),
        title: String::new(),
        menu: Vec::new(),
        icon: crate::tray_host::default_icon(),
    }
}

fn bounded_text(value: &Value, name: &str) -> Result<String, TrayFailure> {
    if value.is_nullish() {
        return Err(tray_failure("TypeError", format!("{name} is required")));
    }
    let text = value.to_js_string();
    if text.contains('\0') || text.chars().count() > MAX_TRAY_TEXT {
        return Err(tray_failure(
            "TypeError",
            format!("{name} must be plain text of at most {MAX_TRAY_TEXT} characters"),
        ));
    }
    Ok(text)
}

fn icon_side(value: &Value, name: &str) -> Result<u32, TrayFailure> {
    let number = value.to_number();
    if !number.is_finite() || number < 1.0 || number > 128.0 || number.fract() != 0.0 {
        return Err(tray_failure(
            "TypeError",
            format!("{name} must be an integer from 1 to 128"),
        ));
    }
    Ok(number as u32)
}

fn parse_icon(value: &Value) -> Result<crate::tray_host::TrayIconPixels, TrayFailure> {
    if !value.is_object() || value.is_array() {
        return Err(tray_failure(
            "TypeError",
            "tray icon must be an object with width, height, and rgba",
        ));
    }
    let width = icon_side(&value.get_property("width"), "icon.width")?;
    let height = icon_side(&value.get_property("height"), "icon.height")?;
    let rgba_value = value.get_property("rgba");
    if !rgba_value.is_array() {
        return Err(tray_failure(
            "TypeError",
            "tray icon rgba must be a sequence of bytes",
        ));
    }
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    let length = rgba_value.get_property("length").to_u32() as usize;
    if length != expected {
        return Err(tray_failure(
            "TypeError",
            "tray icon rgba length must be width × height × 4",
        ));
    }
    let mut rgba = Vec::with_capacity(expected);
    for index in 0..length {
        let component = rgba_value.get_property(&index.to_string()).to_number();
        if !component.is_finite()
            || component < 0.0
            || component > 255.0
            || component.fract() != 0.0
        {
            return Err(tray_failure(
                "TypeError",
                "tray icon rgba values must be integers from 0 to 255",
            ));
        }
        rgba.push(component as u8);
    }
    Ok(crate::tray_host::TrayIconPixels {
        rgba,
        width,
        height,
    })
}

fn parse_menu(value: &Value) -> Result<Vec<TrayMenuEntry>, TrayFailure> {
    if !value.is_array() {
        return Err(tray_failure(
            "TypeError",
            "tray menu must be a sequence of items",
        ));
    }
    let length = value.get_property("length").to_u32() as usize;
    if length > MAX_TRAY_ITEMS {
        return Err(tray_failure(
            "TypeError",
            format!("tray menu is limited to {MAX_TRAY_ITEMS} items"),
        ));
    }
    let mut items = Vec::with_capacity(length);
    for index in 0..length {
        let item = value.get_property(&index.to_string());
        if !item.is_object() || item.is_array() {
            return Err(tray_failure(
                "TypeError",
                "tray menu item must be an object",
            ));
        }
        if !item.get_property("submenu").is_undefined() {
            return Err(tray_failure(
                "TypeError",
                "nested tray menus are not supported",
            ));
        }
        let separator = item.get_property("type").to_js_string() == "separator"
            || (!item.get_property("separator").is_undefined()
                && item.get_property("separator").to_bool());
        if separator {
            items.push(TrayMenuEntry {
                id: String::new(),
                label: String::new(),
                enabled: true,
                checked: false,
                separator: true,
            });
            continue;
        }
        let id = bounded_text(&item.get_property("id"), "menu item id")?;
        let label = bounded_text(&item.get_property("label"), "menu item label")?;
        if id.is_empty() || label.is_empty() {
            return Err(tray_failure(
                "TypeError",
                "tray menu item requires an id and a label",
            ));
        }
        if id.contains('\u{1f}') {
            return Err(tray_failure(
                "TypeError",
                "tray menu item id must be plain text",
            ));
        }
        if items.iter().any(|entry| !entry.separator && entry.id == id) {
            return Err(tray_failure(
                "TypeError",
                "tray menu item ids must be unique",
            ));
        }
        let enabled = if item.get_property("enabled").is_undefined() {
            true
        } else {
            item.get_property("enabled").to_bool()
        };
        items.push(TrayMenuEntry {
            id,
            label,
            enabled,
            checked: item.get_property("checked").to_bool(),
            separator: false,
        });
    }
    Ok(items)
}

fn merge_spec(current: &TraySpec, options: &Value) -> Result<TraySpec, TrayFailure> {
    if options.is_undefined() {
        return Ok(current.clone());
    }
    if !options.is_object() || options.is_array() {
        return Err(tray_failure("TypeError", "tray options must be an object"));
    }
    let mut spec = current.clone();
    if !options.get_property("tooltip").is_undefined() {
        spec.tooltip = bounded_text(&options.get_property("tooltip"), "tooltip")?;
    }
    if !options.get_property("title").is_undefined() {
        spec.title = bounded_text(&options.get_property("title"), "title")?;
    }
    if !options.get_property("menu").is_undefined() {
        spec.menu = parse_menu(&options.get_property("menu"))?;
    }
    if !options.get_property("icon").is_undefined() {
        spec.icon = parse_icon(&options.get_property("icon"))?;
    }
    Ok(spec)
}

fn tray_spec(id: u64) -> Result<TraySpec, TrayFailure> {
    TRAYS.with(|trays| {
        trays
            .borrow()
            .icons
            .get(&id)
            .map(|live| live.spec.clone())
            .ok_or_else(|| tray_failure("InvalidStateError", "tray icon was removed"))
    })
}

fn store_spec(id: u64, spec: TraySpec) {
    TRAYS.with(|trays| {
        if let Some(live) = trays.borrow_mut().icons.get_mut(&id) {
            live.spec = spec;
        }
    });
}

fn remember_tray(id: u64, spec: TraySpec) {
    TRAYS.with(|trays| {
        let mut trays = trays.borrow_mut();
        trays.order.retain(|live| *live != id);
        trays.order.push(id);
        trays.icons.insert(
            id,
            LiveTray {
                spec,
                on_click: None,
                on_menu: None,
            },
        );
    });
    refresh_tray_flag();
}

fn apply_spec(id: u64, spec: TraySpec) -> Result<(), TrayFailure> {
    if tray_spec(id).is_err() {
        return Err(tray_failure("InvalidStateError", "tray icon was removed"));
    }
    crate::tray_host::update(id, &spec).map_err(host_failure)?;
    store_spec(id, spec);
    Ok(())
}

fn active_tray_id() -> Result<u64, TrayFailure> {
    TRAYS.with(|trays| {
        trays.borrow().order.last().copied().ok_or_else(|| {
            tray_failure(
                "InvalidStateError",
                "tray.setMenu requires a tray icon from tray.show()",
            )
        })
    })
}

fn set_tray_callback(id: u64, click: bool, callback: Value) -> Result<(), TrayFailure> {
    if !callback.is_function() {
        let message = if click {
            "tray.onClick requires a function"
        } else {
            "tray.onMenu requires a function"
        };
        return Err(tray_failure("TypeError", message));
    }
    TRAYS.with(|trays| {
        let mut trays = trays.borrow_mut();
        let Some(live) = trays.icons.get_mut(&id) else {
            return Err(tray_failure("InvalidStateError", "tray icon was removed"));
        };
        if click {
            live.on_click = Some(callback);
        } else {
            live.on_menu = Some(callback);
        }
        Ok(())
    })
}

fn remove_tray(id: u64) -> Result<(), TrayFailure> {
    if tray_spec(id).is_err() {
        return Err(tray_failure("InvalidStateError", "tray icon was removed"));
    }
    crate::tray_host::remove(id).map_err(host_failure)?;
    TRAYS.with(|trays| {
        let mut trays = trays.borrow_mut();
        trays.icons.remove(&id);
        trays.order.retain(|live| *live != id);
    });
    refresh_tray_flag();
    if !TRAY_ACTIVE.with(|flag| flag.get()) {
        let _ = crate::tray_host::poll_events();
    }
    Ok(())
}

fn reflect_spec(handle: &Value, spec: &TraySpec) {
    handle.set_property("tooltip", Value::string(&spec.tooltip));
    handle.set_property("title", Value::string(&spec.title));
}

fn tray_handle(generation: u32, id: u64, spec: &TraySpec) -> Value {
    let handle = Value::object(HashMap::from([
        ("id".into(), Value::Number(id as f64)),
        ("tooltip".into(), Value::string(&spec.tooltip)),
        ("title".into(), Value::string(&spec.title)),
        ("removed".into(), Value::Bool(false)),
    ]));
    let stored_update = handle.clone();
    handle.set_property(
        "update",
        realm_function(generation, move |_, args| {
            permit(|| {
                let options = args.first().cloned().unwrap_or(Value::Undefined);
                let spec = merge_spec(&tray_spec(id)?, &options)?;
                apply_spec(id, spec.clone())?;
                reflect_spec(&stored_update, &spec);
                Ok(Value::Undefined)
            })
        }),
    );
    let stored_menu = handle.clone();
    handle.set_property(
        "setMenu",
        realm_function(generation, move |_, args| {
            permit(|| {
                let items = args.first().cloned().unwrap_or(Value::Undefined);
                let mut spec = tray_spec(id)?;
                spec.menu = parse_menu(&items)?;
                apply_spec(id, spec.clone())?;
                reflect_spec(&stored_menu, &spec);
                Ok(Value::Undefined)
            })
        }),
    );
    handle.set_property(
        "onClick",
        realm_function(generation, move |_, args| {
            permit(|| {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                set_tray_callback(id, true, callback)?;
                Ok(Value::Undefined)
            })
        }),
    );
    handle.set_property(
        "onMenu",
        realm_function(generation, move |_, args| {
            permit(|| {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                set_tray_callback(id, false, callback)?;
                Ok(Value::Undefined)
            })
        }),
    );
    let stored_remove = handle.clone();
    handle.set_property(
        "remove",
        realm_function(generation, move |_, _| {
            permit(|| {
                remove_tray(id)?;
                stored_remove.set_property("removed", Value::Bool(true));
                Ok(Value::Undefined)
            })
        }),
    );
    handle
}

fn tray_namespace() -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    value.set_property(
        "show",
        realm_function(generation, move |_, args| {
            permit(|| {
                let options = args.first().cloned().unwrap_or(Value::Undefined);
                let spec = merge_spec(&empty_tray_spec(), &options)?;
                let id = crate::tray_host::create(&spec).map_err(host_failure)?;
                remember_tray(id, spec.clone());
                Ok(tray_handle(generation, id, &spec))
            })
        }),
    );
    value.set_property(
        "setMenu",
        realm_function(generation, move |_, args| {
            permit(|| {
                let id = active_tray_id()?;
                let items = args.first().cloned().unwrap_or(Value::Undefined);
                let mut spec = tray_spec(id)?;
                spec.menu = parse_menu(&items)?;
                apply_spec(id, spec)?;
                Ok(Value::Undefined)
            })
        }),
    );
    value
}

pub fn flush_tray_events() -> usize {
    if !TRAY_ACTIVE.with(|flag| flag.get()) {
        return 0;
    }
    let already = TRAY_FLUSHING.with(|flag| {
        let current = flag.get();
        flag.set(true);
        current
    });
    if already {
        return 0;
    }
    struct ClearFlush;
    impl Drop for ClearFlush {
        fn drop(&mut self) {
            TRAY_FLUSHING.with(|flag| flag.set(false));
        }
    }
    let _clear = ClearFlush;
    let events = crate::tray_host::poll_events();
    let count = events.len();
    let pending = TRAYS.with(|trays| {
        let trays = trays.borrow();
        events
            .into_iter()
            .filter_map(|event| {
                let live = trays.icons.get(&event.id)?;
                let callback = match &event.kind {
                    TrayEventKind::Click => live.on_click.clone(),
                    TrayEventKind::Menu(_) => live.on_menu.clone(),
                }?;
                Some((callback, event.kind))
            })
            .collect::<Vec<_>>()
    });
    for (callback, kind) in pending {
        if !callback.is_function() {
            continue;
        }
        match kind {
            TrayEventKind::Click => {
                callback.call(Value::Undefined, vec![]);
            }
            TrayEventKind::Menu(item) => {
                callback.call(Value::Undefined, vec![Value::string(&item)]);
            }
        }
    }
    count
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

fn keyring_rejected(err: crate::keyring_store::KeyringError) -> Value {
    rejected(err.name(), err.message())
}

fn keyring_namespace() -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    value.set_property(
        "get",
        realm_function(generation, |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_KEYRING)
            {
                return rejected;
            }
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            match crate::keyring_store::get_secret(&name) {
                Ok(Some(secret)) => w3cos_core::promise::resolve(vec![Value::string(&secret)]),
                Ok(None) => w3cos_core::promise::resolve(vec![Value::Null]),
                Err(err) => keyring_rejected(err),
            }
        }),
    );
    value.set_property(
        "set",
        realm_function(generation, |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_KEYRING)
            {
                return rejected;
            }
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            let Some(secret) = args.get(1).filter(|value| !value.is_undefined()) else {
                return rejected("TypeError", "keyring.set requires a secret");
            };
            match crate::keyring_store::set_secret(&name, &secret.to_js_string()) {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(err) => keyring_rejected(err),
            }
        }),
    );
    value.set_property(
        "delete",
        realm_function(generation, |_, args| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_KEYRING)
            {
                return rejected;
            }
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            match crate::keyring_store::delete_secret(&name) {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(err) => keyring_rejected(err),
            }
        }),
    );
    value.set_property(
        "list",
        realm_function(generation, |_, _| {
            if let Some(rejected) =
                crate::capability_gate::reject_unless_granted(PERMISSION_KEYRING)
            {
                return rejected;
            }
            match crate::keyring_store::list_secrets() {
                Ok(names) => w3cos_core::promise::resolve(vec![Value::array(
                    names.into_iter().map(|name| Value::string(&name)).collect(),
                )]),
                Err(err) => keyring_rejected(err),
            }
        }),
    );
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
        ("tray".into(), tray_namespace()),
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
        ("keyring".into(), keyring_namespace()),
        ("agent".into(), agent_namespace()),
    ]))
}

pub fn reset_realm() {
    SHORTCUTS.with(|shortcuts| shortcuts.borrow_mut().clear());
    TRAYS.with(|trays| {
        let mut trays = trays.borrow_mut();
        trays.order.clear();
        trays.icons.clear();
    });
    #[cfg(all(
        any(target_os = "macos", target_os = "linux", target_os = "windows"),
        not(target_env = "ohos")
    ))]
    PROCESSES.with(|processes| processes.borrow_mut().clear());
    refresh_active_flag();
    refresh_tray_flag();
    crate::platform_host::clear_grabs();
    crate::keyring_store::reset_override();
    crate::tray_host::reset();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn await_outcome(promise: Value) -> Result<Value, (String, String)> {
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
                *err_for_catch.borrow_mut() = args.first().map(|value| {
                    (
                        value.get_property("name").to_js_string(),
                        value.get_property("message").to_js_string(),
                    )
                });
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        if let Some(error) = err.borrow().clone() {
            return Err(error);
        }
        ok.borrow()
            .clone()
            .ok_or_else(|| ("unresolved".into(), String::new()))
    }

    fn await_value(promise: Value) -> Result<Value, String> {
        await_outcome(promise).map_err(|(name, _)| name)
    }

    fn menu_item(id: &str, label: &str) -> Value {
        Value::object(HashMap::from([
            ("id".into(), Value::string(id)),
            ("label".into(), Value::string(label)),
        ]))
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
        crate::capability_gate::grant(PERMISSION_SYSTEM);
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

    fn string_items(value: &Value) -> Vec<String> {
        let length = value.get_property("length").to_u32();
        (0..length)
            .map(|index| value.get_property(&index.to_string()).to_js_string())
            .collect()
    }

    #[test]
    fn keyring_requires_permission_and_lists_only_the_current_origin() {
        reset_realm();
        crate::capability_gate::reset();
        crate::history::reset();
        crate::jsdom::reset_bridge();
        crate::keyring_store::install_mock();
        let namespace = namespace_value();
        let keyring = namespace.get_property("keyring");
        let denied = await_value(keyring.call_method(
            "set",
            vec![Value::string("token"), Value::string("super-secret-value")],
        ))
        .unwrap_err();
        assert_eq!(denied, "NotAllowedError");

        crate::capability_gate::grant(PERMISSION_KEYRING);
        crate::history::location_replace("https://app.example/");
        await_value(keyring.call_method(
            "set",
            vec![Value::string("token"), Value::string("super-secret-value")],
        ))
        .unwrap();
        await_value(keyring.call_method(
            "set",
            vec![Value::string("api token.v2"), Value::string("dotted")],
        ))
        .unwrap();
        let stored = await_value(keyring.call_method("get", vec![Value::string("token")])).unwrap();
        assert_eq!(stored.to_js_string(), "super-secret-value");
        let names = await_value(keyring.call_method("list", vec![])).unwrap();
        assert_eq!(
            string_items(&names),
            vec!["api token.v2".to_string(), "token".to_string()]
        );

        crate::history::location_replace("https://app.example.evil/");
        let foreign =
            await_value(keyring.call_method("get", vec![Value::string("token")])).unwrap();
        assert!(foreign.is_null());
        let foreign_names = await_value(keyring.call_method("list", vec![])).unwrap();
        assert_eq!(foreign_names.get_property("length").to_u32(), 0);
        await_value(keyring.call_method(
            "set",
            vec![Value::string("token"), Value::string("other-secret")],
        ))
        .unwrap();

        crate::history::location_replace("https://app.example/");
        let still = await_value(keyring.call_method("get", vec![Value::string("token")])).unwrap();
        assert_eq!(still.to_js_string(), "super-secret-value");
        await_value(keyring.call_method("delete", vec![Value::string("token")])).unwrap();
        await_value(keyring.call_method("delete", vec![Value::string("token")])).unwrap();
        let gone = await_value(keyring.call_method("get", vec![Value::string("token")])).unwrap();
        assert!(gone.is_null());

        let missing_secret =
            await_value(keyring.call_method("set", vec![Value::string("token")])).unwrap_err();
        assert_eq!(missing_secret, "TypeError");

        crate::keyring_store::install_unavailable(
            "navigator.w3cos.keyring cannot reach the system secret store (no D-Bus session bus; Secret Service is unavailable); the call was refused",
        );
        let refused = await_value(keyring.call_method(
            "set",
            vec![Value::string("token"), Value::string("super-secret-value")],
        ))
        .unwrap_err();
        assert_eq!(refused, "NotSupportedError");
        crate::history::reset();
        crate::capability_gate::reset();
        reset_realm();
    }

    #[test]
    fn tray_icon_menu_click_and_remove_do_not_need_a_display() {
        reset_realm();
        crate::capability_gate::reset();
        crate::tray_host::install_recorder();
        let namespace = namespace_value();
        let denied =
            await_value(namespace.get_property("tray").call_method("show", vec![])).unwrap_err();
        assert_eq!(denied, "NotAllowedError");

        crate::capability_gate::grant(PERMISSION_TRAY);
        let handle = await_value(namespace.get_property("tray").call_method(
            "show",
            vec![Value::object(HashMap::from([
                ("tooltip".into(), Value::string("Build")),
                ("title".into(), Value::string("W3C OS")),
                (
                    "menu".into(),
                    Value::array(vec![
                        menu_item("quit", "Quit"),
                        Value::object(HashMap::from([("type".into(), Value::string("separator"))])),
                        Value::object(HashMap::from([
                            ("id".into(), Value::string("pause")),
                            ("label".into(), Value::string("Pause")),
                            ("checked".into(), Value::Bool(true)),
                            ("enabled".into(), Value::Bool(false)),
                        ])),
                    ]),
                ),
            ]))],
        ))
        .unwrap();
        let id = handle.get_property("id").to_number() as u64;
        let spec = crate::tray_host::recorder_spec(id).expect("recorded icon");
        assert_eq!(spec.tooltip, "Build");
        assert_eq!(spec.title, "W3C OS");
        assert_eq!(spec.icon.width, 16);
        assert_eq!(spec.icon.rgba.len(), 16 * 16 * 4);
        assert_eq!(spec.menu.len(), 3);
        assert!(spec.menu[1].separator);
        assert!(spec.menu[2].checked);
        assert!(!spec.menu[2].enabled);
        assert!(
            namespace
                .get_property("tray")
                .get_property("setMenu")
                .is_function()
        );

        await_value(handle.call_method(
            "update",
            vec![Value::object(HashMap::from([(
                "tooltip".into(),
                Value::string("Updated"),
            )]))],
        ))
        .unwrap();
        assert_eq!(
            crate::tray_host::recorder_spec(id).unwrap().tooltip,
            "Updated"
        );
        assert_eq!(handle.get_property("tooltip").to_js_string(), "Updated");
        assert_eq!(crate::tray_host::recorder_spec(id).unwrap().menu.len(), 3);

        await_value(handle.call_method(
            "setMenu",
            vec![Value::array(vec![menu_item("open", "Open")])],
        ))
        .unwrap();
        assert_eq!(
            crate::tray_host::recorder_spec(id).unwrap().menu[0].id,
            "open"
        );

        await_value(namespace.get_property("tray").call_method(
            "setMenu",
            vec![Value::array(vec![menu_item("about", "About")])],
        ))
        .unwrap();
        assert_eq!(
            crate::tray_host::recorder_spec(id).unwrap().menu[0].id,
            "about"
        );

        let clicks = Rc::new(RefCell::new(0u32));
        let clicks_for_callback = Rc::clone(&clicks);
        await_value(handle.call_method(
            "onClick",
            vec![Value::function(move |_, _| {
                *clicks_for_callback.borrow_mut() += 1;
                Value::Undefined
            })],
        ))
        .unwrap();
        let chosen = Rc::new(RefCell::new(String::new()));
        let chosen_for_callback = Rc::clone(&chosen);
        await_value(handle.call_method(
            "onMenu",
            vec![Value::function(move |_, args| {
                *chosen_for_callback.borrow_mut() =
                    args.first().map(Value::to_js_string).unwrap_or_default();
                Value::Undefined
            })],
        ))
        .unwrap();
        crate::tray_host::push_recorder_event(id, crate::tray_host::TrayEventKind::Click);
        crate::tray_host::push_recorder_event(
            id,
            crate::tray_host::TrayEventKind::Menu("about".into()),
        );
        assert_eq!(flush_tray_events(), 2);
        assert_eq!(*clicks.borrow(), 1);
        assert_eq!(chosen.borrow().as_str(), "about");

        let bad_icon = await_value(handle.call_method(
            "update",
            vec![Value::object(HashMap::from([(
                "icon".into(),
                Value::object(HashMap::from([
                    ("width".into(), Value::Number(1.0)),
                    ("height".into(), Value::Number(1.0)),
                    ("rgba".into(), Value::array(vec![Value::Number(255.0)])),
                ])),
            )]))],
        ))
        .unwrap_err();
        assert_eq!(bad_icon, "TypeError");
        assert_eq!(
            crate::tray_host::recorder_spec(id).unwrap().tooltip,
            "Updated"
        );

        let nested = await_outcome(namespace.get_property("tray").call_method(
            "show",
            vec![Value::object(HashMap::from([(
                "menu".into(),
                Value::array(vec![Value::object(HashMap::from([
                    ("id".into(), Value::string("file")),
                    ("label".into(), Value::string("File")),
                    ("submenu".into(), Value::array(vec![])),
                ]))]),
            )]))],
        ))
        .unwrap_err();
        assert_eq!(nested.0, "TypeError");
        assert!(nested.1.contains("nested"));

        await_value(handle.call_method("remove", vec![])).unwrap();
        assert!(handle.get_property("removed").to_bool());
        assert!(crate::tray_host::recorder_spec(id).is_none());
        crate::tray_host::push_recorder_event(id, crate::tray_host::TrayEventKind::Click);
        assert_eq!(flush_tray_events(), 0);
        assert_eq!(*clicks.borrow(), 1);
        let removed = await_value(handle.call_method(
            "update",
            vec![Value::object(HashMap::from([(
                "tooltip".into(),
                Value::string("gone"),
            )]))],
        ))
        .unwrap_err();
        assert_eq!(removed, "InvalidStateError");
        let no_icon = await_value(
            namespace
                .get_property("tray")
                .call_method("setMenu", vec![Value::array(vec![])]),
        )
        .unwrap_err();
        assert_eq!(no_icon, "InvalidStateError");

        crate::capability_gate::reset();
        reset_realm();
    }

    #[test]
    fn tray_show_reaches_the_real_adapter_without_a_display() {
        reset_realm();
        crate::capability_gate::reset();
        crate::capability_gate::grant(PERMISSION_TRAY);
        let namespace = namespace_value();
        let shown = await_outcome(namespace.get_property("tray").call_method(
            "show",
            vec![Value::object(HashMap::from([(
                "tooltip".into(),
                Value::string("w3cos-native"),
            )]))],
        ));
        match shown {
            Ok(handle) => {
                await_value(handle.call_method("remove", vec![])).expect("remove live tray icon");
            }
            Err((name, message)) => {
                assert_eq!(name, "NotSupportedError");
                let backend = crate::tray_host::backend_name();
                assert_ne!(backend, "none", "{message}");
                assert!(
                    message.contains(backend),
                    "adapter error should name {backend}: {message}"
                );
                assert!(
                    !message.contains("no platform adapter"),
                    "desktop builds must call the real adapter: {message}"
                );
            }
        }
        crate::capability_gate::reset();
        reset_realm();
    }
}
