//! iOS UI-test hook — HTTP snapshot, perf metrics, bench driver (`:19090`).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, Once};
use std::thread;
use winit::event_loop::EventLoopProxy;

const MAX_PENDING_COMMANDS: usize = 256;
static SERVER_ONCE: Once = Once::new();
static SNAPSHOT: Mutex<Option<String>> = Mutex::new(None);
static HIT_TARGETS: Mutex<Vec<UiHitTarget>> = Mutex::new(Vec::new());
static INPUT_TARGETS: Mutex<Vec<UiInputTarget>> = Mutex::new(Vec::new());
static EVENT_LOOP_PROXY: Mutex<Option<EventLoopProxy<()>>> = Mutex::new(None);
static PENDING_COMMANDS: Mutex<VecDeque<PendingCommand>> = Mutex::new(VecDeque::new());
static NEEDS_REPAINT: AtomicBool = AtomicBool::new(false);
static BENCH_REPAINTS: AtomicI64 = AtomicI64::new(0);
static CONSUMED_ACTIONS: AtomicI64 = AtomicI64::new(0);
static APPLIED_ACTIONS: AtomicI64 = AtomicI64::new(0);
static CONSUMED_SCROLLS: AtomicI64 = AtomicI64::new(0);
static APPLIED_SCROLLS: AtomicI64 = AtomicI64::new(0);
static CONSUMED_CLICKS: AtomicI64 = AtomicI64::new(0);
static APPLIED_CLICKS: AtomicI64 = AtomicI64::new(0);
static CONSUMED_INPUTS: AtomicI64 = AtomicI64::new(0);
static APPLIED_INPUTS: AtomicI64 = AtomicI64::new(0);
static CONSUMED_REPAINTS: AtomicI64 = AtomicI64::new(0);
static FOCUSED_INDEX: AtomicI64 = AtomicI64::new(-1);
static POINTER_X_MILLI: AtomicI64 = AtomicI64::new(-1);
static POINTER_Y_MILLI: AtomicI64 = AtomicI64::new(-1);
static PRESSED_INDEX: AtomicI64 = AtomicI64::new(-1);
static NATIVE_FIRST_RESPONDER: AtomicI64 = AtomicI64::new(-1);
static NATIVE_KEY_WINDOW: AtomicI64 = AtomicI64::new(-1);
static LAST_SCROLL_INDEX: AtomicI64 = AtomicI64::new(-1);
static LAST_SCROLL_Y_MILLI: AtomicI64 = AtomicI64::new(0);
static LAST_SCROLL_SOURCE: AtomicI64 = AtomicI64::new(0);
static LAST_SCROLL_REQUEST_Y_MILLI: AtomicI64 = AtomicI64::new(i64::MIN);
static LAST_RELEASE_VELOCITY_MILLI: AtomicI64 = AtomicI64::new(0);
static KINETIC_ACTIVE: AtomicBool = AtomicBool::new(false);
static KINETIC_TICKS: AtomicI64 = AtomicI64::new(0);
static KINETIC_ELAPSED_MILLI: AtomicI64 = AtomicI64::new(0);
static KINETIC_DELTA_MILLI: AtomicI64 = AtomicI64::new(0);
static KINETIC_SAMPLE_VELOCITY_MILLI: AtomicI64 = AtomicI64::new(0);
static KINETIC_CURVE_ACTIVE: AtomicBool = AtomicBool::new(false);
static KINETIC_CONTINUED: AtomicBool = AtomicBool::new(false);
static KINETIC_NODE_INDEX: AtomicI64 = AtomicI64::new(-1);
static KINETIC_NODE_MAX_MILLI: AtomicI64 = AtomicI64::new(-1);
static KINETIC_NODE_OFFSET_MILLI: AtomicI64 = AtomicI64::new(-1);
static KINETIC_APPLIED_MILLI: AtomicI64 = AtomicI64::new(0);

#[derive(Clone, serde::Serialize)]
pub struct UiHitTarget {
    pub index: usize,
    pub label: String,
    pub action: String,
    pub cx: f32,
    pub cy: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PendingCommand {
    Action(String),
    Scroll(f32),
    Click(usize),
    Input { index: usize, value: String },
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
        "lastScroll": {
            "index": match LAST_SCROLL_INDEX.load(Ordering::SeqCst) {
                -1 => serde_json::Value::Null,
                index => serde_json::json!(index),
            },
            "y": LAST_SCROLL_Y_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "source": match LAST_SCROLL_SOURCE.load(Ordering::SeqCst) {
                1 => "programmatic",
                2 => "anchor",
                _ => "direct",
            },
            "requestedY": match LAST_SCROLL_REQUEST_Y_MILLI.load(Ordering::SeqCst) {
                i64::MIN => serde_json::Value::Null,
                value => serde_json::json!(value as f64 / 1000.0),
            },
        },
        "kinetic": {
            "releaseVelocity": LAST_RELEASE_VELOCITY_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "active": KINETIC_ACTIVE.load(Ordering::SeqCst),
            "ticks": KINETIC_TICKS.load(Ordering::SeqCst),
            "elapsed": KINETIC_ELAPSED_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "delta": KINETIC_DELTA_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "sampleVelocity": KINETIC_SAMPLE_VELOCITY_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "curveActive": KINETIC_CURVE_ACTIVE.load(Ordering::SeqCst),
            "continued": KINETIC_CONTINUED.load(Ordering::SeqCst),
            "nodeIndex": KINETIC_NODE_INDEX.load(Ordering::SeqCst),
            "nodeMax": KINETIC_NODE_MAX_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "nodeOffset": KINETIC_NODE_OFFSET_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
            "applied": KINETIC_APPLIED_MILLI.load(Ordering::SeqCst) as f64 / 1000.0,
        },
    })
    .to_string()
}

pub fn set_kinetic_attempt(index: usize, max_y: Option<f32>, offset_y: Option<f32>, applied: f32) {
    if hook_enabled() {
        KINETIC_NODE_INDEX.store(index as i64, Ordering::SeqCst);
        KINETIC_NODE_MAX_MILLI.store(
            max_y.map(|v| (v * 1000.0) as i64).unwrap_or(-1),
            Ordering::SeqCst,
        );
        KINETIC_NODE_OFFSET_MILLI.store(
            offset_y.map(|v| (v * 1000.0) as i64).unwrap_or(-1),
            Ordering::SeqCst,
        );
        KINETIC_APPLIED_MILLI.store((applied * 1000.0) as i64, Ordering::SeqCst);
    }
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

pub fn set_scroll_offset(index: usize, y: f32) {
    set_scroll_offset_source(index, y, 0, None);
}

pub fn set_programmatic_scroll_offset(index: usize, y: f32, requested_y: Option<f32>) {
    set_scroll_offset_source(index, y, 1, requested_y);
}

pub fn set_anchor_scroll_offset(index: usize, y: f32) {
    set_scroll_offset_source(index, y, 2, None);
}

fn set_scroll_offset_source(index: usize, y: f32, source: i64, requested_y: Option<f32>) {
    if hook_enabled() {
        LAST_SCROLL_INDEX.store(index as i64, Ordering::SeqCst);
        LAST_SCROLL_Y_MILLI.store((y * 1000.0) as i64, Ordering::SeqCst);
        LAST_SCROLL_SOURCE.store(source, Ordering::SeqCst);
        LAST_SCROLL_REQUEST_Y_MILLI.store(
            requested_y
                .map(|value| (value * 1000.0) as i64)
                .unwrap_or(i64::MIN),
            Ordering::SeqCst,
        );
    }
}

pub fn set_kinetic_started(velocity: f32) {
    if hook_enabled() {
        LAST_RELEASE_VELOCITY_MILLI.store((velocity * 1000.0) as i64, Ordering::SeqCst);
        KINETIC_TICKS.store(0, Ordering::SeqCst);
        KINETIC_ACTIVE.store(true, Ordering::SeqCst);
    }
}

pub fn set_kinetic_tick(
    active: bool,
    elapsed: f32,
    delta: f32,
    sample_velocity: f32,
    curve_active: bool,
    continued: bool,
) {
    if hook_enabled() {
        KINETIC_TICKS.fetch_add(1, Ordering::SeqCst);
        KINETIC_ACTIVE.store(active, Ordering::SeqCst);
        KINETIC_ELAPSED_MILLI.store((elapsed * 1000.0) as i64, Ordering::SeqCst);
        KINETIC_DELTA_MILLI.store((delta * 1000.0) as i64, Ordering::SeqCst);
        KINETIC_SAMPLE_VELOCITY_MILLI.store((sample_velocity * 1000.0) as i64, Ordering::SeqCst);
        KINETIC_CURVE_ACTIVE.store(curve_active, Ordering::SeqCst);
        KINETIC_CONTINUED.store(continued, Ordering::SeqCst);
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

pub fn set_event_loop_proxy(proxy: EventLoopProxy<()>) {
    if let Ok(mut slot) = EVENT_LOOP_PROXY.lock() {
        *slot = Some(proxy);
    }
}

fn wake_event_loop() {
    if let Ok(slot) = EVENT_LOOP_PROXY.lock()
        && let Some(proxy) = slot.as_ref()
    {
        let _ = proxy.send_event(());
    }
}

fn queue_command(command: PendingCommand) {
    if let Ok(mut pending) = PENDING_COMMANDS.lock() {
        if pending.len() == MAX_PENDING_COMMANDS {
            pending.pop_front();
        }
        pending.push_back(command);
    }
    NEEDS_REPAINT.store(true, Ordering::SeqCst);
    wake_event_loop();
}

fn queue_action(action: impl Into<String>) {
    queue_command(PendingCommand::Action(action.into()));
}

fn queue_scroll(dy: f32) {
    queue_command(PendingCommand::Scroll(dy));
}

fn queue_click(index: usize) {
    queue_command(PendingCommand::Click(index));
}

fn queue_input(index: usize, value: impl Into<String>) {
    queue_command(PendingCommand::Input {
        index,
        value: value.into(),
    });
}

pub(crate) fn drain_pending_commands(limit: usize) -> Vec<PendingCommand> {
    if !hook_enabled() {
        return Vec::new();
    }
    let Ok(mut pending) = PENDING_COMMANDS.lock() else {
        return Vec::new();
    };
    let count = pending.len().min(limit);
    pending.drain(..count).collect()
}

pub(crate) fn record_command_result(command: &PendingCommand, applied: bool) {
    let (consumed, successful) = match command {
        PendingCommand::Action(_) => (&CONSUMED_ACTIONS, &APPLIED_ACTIONS),
        PendingCommand::Scroll(_) => (&CONSUMED_SCROLLS, &APPLIED_SCROLLS),
        PendingCommand::Click(_) => (&CONSUMED_CLICKS, &APPLIED_CLICKS),
        PendingCommand::Input { .. } => (&CONSUMED_INPUTS, &APPLIED_INPUTS),
    };
    consumed.fetch_add(1, Ordering::SeqCst);
    if applied {
        successful.fetch_add(1, Ordering::SeqCst);
    }
}

pub fn take_repaint_request() -> bool {
    NEEDS_REPAINT.swap(false, Ordering::SeqCst)
}

/// Consume one requested benchmark repaint for one distinct event-loop turn.
pub fn take_bench_repaint() -> bool {
    if !hook_enabled() {
        return false;
    }
    let consumed = BENCH_REPAINTS
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |pending| {
            (pending > 0).then_some(pending - 1)
        })
        .is_ok();
    if consumed {
        CONSUMED_REPAINTS.fetch_add(1, Ordering::SeqCst);
    }
    consumed
}

pub fn queue_bench_repaints(n: u32) {
    BENCH_REPAINTS.store(n as i64, Ordering::SeqCst);
    NEEDS_REPAINT.store(true, Ordering::SeqCst);
    wake_event_loop();
}

pub fn reset_scenario_evidence() {
    for counter in [
        &CONSUMED_ACTIONS,
        &APPLIED_ACTIONS,
        &CONSUMED_SCROLLS,
        &APPLIED_SCROLLS,
        &CONSUMED_CLICKS,
        &APPLIED_CLICKS,
        &CONSUMED_INPUTS,
        &APPLIED_INPUTS,
        &CONSUMED_REPAINTS,
    ] {
        counter.store(0, Ordering::SeqCst);
    }
}

pub fn scenario_evidence_json() -> serde_json::Value {
    serde_json::json!({
        "consumed_actions": CONSUMED_ACTIONS.load(Ordering::SeqCst),
        "applied_actions": APPLIED_ACTIONS.load(Ordering::SeqCst),
        "consumed_scrolls": CONSUMED_SCROLLS.load(Ordering::SeqCst),
        "applied_scrolls": APPLIED_SCROLLS.load(Ordering::SeqCst),
        "consumed_clicks": CONSUMED_CLICKS.load(Ordering::SeqCst),
        "applied_clicks": APPLIED_CLICKS.load(Ordering::SeqCst),
        "consumed_inputs": CONSUMED_INPUTS.load(Ordering::SeqCst),
        "applied_inputs": APPLIED_INPUTS.load(Ordering::SeqCst),
        "consumed_repaints": CONSUMED_REPAINTS.load(Ordering::SeqCst),
    })
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
    PENDING_COMMANDS
        .lock()
        .ok()
        .map(|g| !g.is_empty())
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

    if let Some(index) = path
        .strip_prefix("/click/")
        .and_then(|s| s.parse::<usize>().ok())
    {
        queue_click(index);
        let body = format!(r#"{{"ok":true,"index":{index}}}"#);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if let Some(name) = path.strip_prefix("/scenario/") {
        reset_scenario_evidence();
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
            reset_scenario_evidence();
            crate::perf::set_scenario(name);
            ("200 OK", format!(r#"{{"scenario":"{name}"}}"#))
        }
        ("POST", p) if p.starts_with("/input/") => {
            let index = p.trim_start_matches("/input/").parse::<usize>();
            let value = request_body(&req);
            match index {
                Ok(index) if !value.is_empty() => {
                    queue_input(index, value);
                    ("200 OK", format!(r#"{{"ok":true,"index":{index}}}"#))
                }
                _ => (
                    "400 Bad Request",
                    r#"{"error":"invalid input target or empty value"}"#.into(),
                ),
            }
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
        _ => ("200 OK", build_snapshot_json()),
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

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_pending() {
        crate::perf::force_enable();
        PENDING_COMMANDS.lock().unwrap().clear();
        BENCH_REPAINTS.store(0, Ordering::SeqCst);
        NEEDS_REPAINT.store(false, Ordering::SeqCst);
        reset_scenario_evidence();
    }

    #[test]
    fn commands_preserve_cross_type_fifo_order() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_pending();

        queue_action("increment:0");
        queue_scroll(-120.0);
        queue_click(17);
        queue_input(23, "benchmark");

        assert_eq!(
            drain_pending_commands(8),
            vec![
                PendingCommand::Action("increment:0".into()),
                PendingCommand::Scroll(-120.0),
                PendingCommand::Click(17),
                PendingCommand::Input {
                    index: 23,
                    value: "benchmark".into(),
                },
            ]
        );
    }

    #[test]
    fn benchmark_repaints_are_consumed_one_turn_at_a_time() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_pending();

        queue_bench_repaints(2);
        assert!(take_bench_repaint());
        assert!(take_bench_repaint());
        assert!(!take_bench_repaint());
        assert_eq!(scenario_evidence_json()["consumed_repaints"], 2);
    }
}
