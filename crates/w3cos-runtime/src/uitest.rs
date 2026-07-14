//! iOS UI-test hook — HTTP snapshot, perf metrics, bench driver (`:19090`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, Once};
use std::thread;

static SERVER_ONCE: Once = Once::new();
static SNAPSHOT: Mutex<Option<String>> = Mutex::new(None);
static HIT_TARGETS: Mutex<Vec<UiHitTarget>> = Mutex::new(Vec::new());
static INPUT_TARGETS: Mutex<Vec<UiInputTarget>> = Mutex::new(Vec::new());
static PENDING_ACTION: Mutex<Option<String>> = Mutex::new(None);
static PENDING_SCROLL_DY: AtomicI64 = AtomicI64::new(i64::MAX);
static NEEDS_REPAINT: AtomicBool = AtomicBool::new(false);
static BENCH_REPAINTS: AtomicI64 = AtomicI64::new(0);
static FOCUSED_INDEX: AtomicI64 = AtomicI64::new(-1);
static POINTER_X_MILLI: AtomicI64 = AtomicI64::new(-1);
static POINTER_Y_MILLI: AtomicI64 = AtomicI64::new(-1);
static PRESSED_INDEX: AtomicI64 = AtomicI64::new(-1);
static NATIVE_FIRST_RESPONDER: AtomicI64 = AtomicI64::new(-1);
static NATIVE_KEY_WINDOW: AtomicI64 = AtomicI64::new(-1);

#[derive(Clone, serde::Serialize)]
pub struct UiHitTarget {
    pub action: String,
    pub cx: f32,
    pub cy: f32,
}

#[derive(Clone, serde::Serialize)]
pub struct UiInputTarget {
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

fn hook_enabled() -> bool {
    std::env::var("W3COS_UITEST").ok().as_deref() == Some("1") || crate::perf::enabled()
}

fn build_snapshot_json() -> String {
    let signals = crate::state::all_signal_values();
    let hist_len = crate::history::get_length();
    let pathname = crate::history::get_pathname();
    let targets = HIT_TARGETS
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    let input_targets = INPUT_TARGETS
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    serde_json::json!({
        "signals": signals,
        "histLen": hist_len,
        "pathname": pathname,
        "targets": targets,
        "inputTargets": input_targets,
        "focusedIndex": match FOCUSED_INDEX.load(Ordering::SeqCst) {
            -1 => serde_json::Value::Null,
            index => serde_json::json!(index),
        },
        "lastPointer": {
            "x": POINTER_X_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "y": POINTER_Y_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "pressedIndex": match PRESSED_INDEX.load(Ordering::SeqCst) {
                -1 => serde_json::Value::Null,
                index => serde_json::json!(index),
            },
        },
        "nativeFirstResponder": match NATIVE_FIRST_RESPONDER.load(Ordering::SeqCst) {
            -1 => serde_json::Value::Null,
            value => serde_json::json!(value == 1),
        },
        "nativeKeyWindow": match NATIVE_KEY_WINDOW.load(Ordering::SeqCst) {
            -1 => serde_json::Value::Null,
            value => serde_json::json!(value == 1),
        },
    })
    .to_string()
}

fn latest_snapshot_json() -> String {
    SNAPSHOT
        .lock()
        .ok()
        .and_then(|snapshot| snapshot.clone())
        .unwrap_or_else(build_snapshot_json)
}

pub fn set_focused_index(index: Option<usize>) {
    if hook_enabled() {
        FOCUSED_INDEX.store(
            index.map(|value| value as i64).unwrap_or(-1),
            Ordering::SeqCst,
        );
    }
}

pub fn set_pointer_hit(x: f32, y: f32, index: Option<usize>) {
    if hook_enabled() {
        POINTER_X_MILLI.store((x * 1000.0) as i64, Ordering::SeqCst);
        POINTER_Y_MILLI.store((y * 1000.0) as i64, Ordering::SeqCst);
        PRESSED_INDEX.store(
            index.map(|value| value as i64).unwrap_or(-1),
            Ordering::SeqCst,
        );
    }
}

pub fn set_native_first_responder(value: Option<bool>) {
    if hook_enabled() {
        NATIVE_FIRST_RESPONDER.store(
            value.map(|is_first| i64::from(is_first)).unwrap_or(-1),
            Ordering::SeqCst,
        );
    }
}

pub fn set_native_key_window(value: Option<bool>) {
    if hook_enabled() {
        NATIVE_KEY_WINDOW.store(
            value.map(|is_key| i64::from(is_key)).unwrap_or(-1),
            Ordering::SeqCst,
        );
    }
}

pub fn set_hit_targets(targets: Vec<UiHitTarget>) {
    if !hook_enabled() {
        return;
    }
    if let Ok(mut cache) = HIT_TARGETS.lock() {
        *cache = targets;
    }
}

pub fn set_input_targets(targets: Vec<UiInputTarget>) {
    if !hook_enabled() {
        return;
    }
    if let Ok(mut cache) = INPUT_TARGETS.lock() {
        *cache = targets;
    }
}

pub fn write_snapshot() {
    if !hook_enabled() {
        return;
    }
    let json = build_snapshot_json();
    if let Ok(mut cache) = SNAPSHOT.lock() {
        *cache = Some(json.clone());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let path = std::path::PathBuf::from(home).join("Documents/w3cos-uitest.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, json);
}

fn route_path(route: i64) -> &'static str {
    match route {
        0 => "/home",
        1 => "/routing",
        2 => "/css",
        3 => "/anim",
        4 => "/network",
        5 => "/layout",
        _ => "/",
    }
}

fn queue_action(action: impl Into<String>) {
    if let Ok(mut pending) = PENDING_ACTION.lock() {
        *pending = Some(action.into());
    }
    NEEDS_REPAINT.store(true, Ordering::SeqCst);
}

fn queue_scroll(dy: f32) {
    PENDING_SCROLL_DY.store(dy as i64, Ordering::SeqCst);
    NEEDS_REPAINT.store(true, Ordering::SeqCst);
}

pub fn drain_pending_action() -> Option<String> {
    if !hook_enabled() {
        return None;
    }
    PENDING_ACTION.lock().ok()?.take()
}

pub fn drain_pending_scroll_dy() -> Option<f32> {
    if !hook_enabled() {
        return None;
    }
    let v = PENDING_SCROLL_DY.swap(i64::MAX, Ordering::SeqCst);
    if v == i64::MAX { None } else { Some(v as f32) }
}

pub fn take_repaint_request() -> bool {
    NEEDS_REPAINT.swap(false, Ordering::SeqCst)
}

/// Force N synchronous paints on the UI thread (bench sampling), batched per frame.
pub fn take_bench_repaints() -> u32 {
    if !hook_enabled() {
        return 0;
    }
    let pending = BENCH_REPAINTS.load(Ordering::SeqCst);
    if pending <= 0 {
        return 0;
    }
    let batch = pending.min(12);
    let remaining = pending - batch;
    BENCH_REPAINTS.store(remaining, Ordering::SeqCst);
    if remaining > 0 {
        NEEDS_REPAINT.store(true, Ordering::SeqCst);
    }
    batch as u32
}

pub fn queue_bench_repaints(n: u32) {
    BENCH_REPAINTS.store(n as i64, Ordering::SeqCst);
    NEEDS_REPAINT.store(true, Ordering::SeqCst);
}

/// UITest HTTP runs off the UI thread; poll the event loop while the hook is active.
pub fn wants_event_loop_poll() -> bool {
    hook_enabled()
}

pub fn has_pending_input() -> bool {
    if !hook_enabled() {
        return false;
    }
    if NEEDS_REPAINT.load(Ordering::SeqCst) {
        return true;
    }
    if BENCH_REPAINTS.load(Ordering::SeqCst) > 0 {
        return true;
    }
    if PENDING_SCROLL_DY.load(Ordering::SeqCst) != i64::MAX {
        return true;
    }
    PENDING_ACTION
        .lock()
        .ok()
        .map(|g| g.is_some())
        .unwrap_or(false)
}

fn parse_request_line(req: &str) -> (&str, &str) {
    let line = req.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");
    (method, path)
}

fn request_body(req: &str) -> &str {
    req.split_once("\r\n\r\n")
        .map(|(_, body)| body.trim())
        .unwrap_or("")
}

fn handle_client(mut stream: TcpStream) {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let (method, path) = parse_request_line(&req);

    if let Some(route) = path.strip_prefix("/navigate/").and_then(|s| {
        s.trim_end_matches('/')
            .split('/')
            .next()
            .and_then(|v| v.parse::<i64>().ok())
    }) {
        queue_action(format!("history:push:route:{route}:{}", route_path(route)));
        let body = format!(r#"{{"ok":true,"route":{route}}}"#);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if let Some(action) = path.strip_prefix("/action/") {
        queue_action(action);
        let body = r#"{"ok":true}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if let Some(dy) = path
        .strip_prefix("/scroll/")
        .and_then(|s| s.parse::<f32>().ok())
    {
        queue_scroll(dy);
        let body = format!(r#"{{"dy":{dy}}}"#);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if let Some(name) = path.strip_prefix("/scenario/") {
        crate::perf::set_scenario(name);
        let body = format!(r#"{{"scenario":"{name}"}}"#);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if let Some(n) = path
        .strip_prefix("/repaint/")
        .and_then(|s| s.parse::<u32>().ok())
    {
        queue_bench_repaints(n);
        let body = format!(r#"{{"repaint":{n}}}"#);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let (status, body) = match (method, path) {
        ("GET", "/metrics") => ("200 OK", crate::perf::summary_json().to_string()),
        ("POST", p) if p.starts_with("/scenario/") => {
            let name = p.trim_start_matches("/scenario/");
            crate::perf::set_scenario(name);
            ("200 OK", format!(r#"{{"scenario":"{name}"}}"#))
        }
        ("POST", "/action") => {
            let action = request_body(&req);
            if action.is_empty() {
                ("400 Bad Request", r#"{"error":"empty action"}"#.into())
            } else {
                queue_action(action);
                ("200 OK", r#"{"ok":true}"#.into())
            }
        }
        ("POST", "/scroll") => {
            let body = request_body(&req);
            let dy = body.trim().parse::<f32>().unwrap_or_else(|_| {
                body.split('=')
                    .nth(1)
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(-120.0)
            });
            queue_scroll(dy);
            ("200 OK", format!(r#"{{"dy":{dy}}}"#))
        }
        _ => ("200 OK", latest_snapshot_json()),
    };

    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
}

pub fn maybe_start_server() {
    if !hook_enabled() {
        return;
    }
    crate::perf::force_enable();
    SERVER_ONCE.call_once(|| {
        thread::spawn(|| {
            let Ok(listener) = TcpListener::bind("0.0.0.0:19090") else {
                return;
            };
            for stream in listener.incoming().flatten() {
                handle_client(stream);
            }
        });
    });
}
