//! MCP adapter over the existing AI Bridge HTTP API.
//!
//! `tools/call` proposes. It does not read the DOM, click, or type. The
//! window arms the one stored call, and the UI thread performs it later.
//! Direct HTTP routes are a different path and still run when the token
//! allows them. Confirm is not a tool and `w3cos/confirm` is not a method.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCall {
    pub method: &'static str,
    pub path: String,
    pub body: Option<String>,
}

/// Map one MCP tool call onto a bridge request.
pub fn tool_to_bridge(name: &str, arguments: &Value) -> Result<BridgeCall, String> {
    let args = if arguments.is_null() {
        &Value::Null
    } else {
        arguments
    };
    match name {
        "observe" => Ok(BridgeCall {
            method: "GET",
            path: "/a11y".into(),
            body: None,
        }),
        "a11y_index" => Ok(BridgeCall {
            method: "GET",
            path: "/a11y/index".into(),
            body: None,
        }),
        "query" => {
            let selector = required_str(args, "selector")?;
            Ok(BridgeCall {
                method: "GET",
                path: format!("/query?selector={}", url_encode(&selector)),
                body: None,
            })
        }
        "click" => Ok(BridgeCall {
            method: "POST",
            path: "/click".into(),
            body: Some(target_body(args)?),
        }),
        "type" => {
            let mut body: Value = serde_json::from_str(&target_body(args)?).unwrap_or(Value::Null);
            body["text"] = Value::String(required_str(args, "text")?);
            Ok(BridgeCall {
                method: "POST",
                path: "/type".into(),
                body: Some(body.to_string()),
            })
        }
        "screenshot" => Ok(BridgeCall {
            method: "GET",
            path: "/screenshot".into(),
            body: None,
        }),
        "screenshot_annotated" => Ok(BridgeCall {
            method: "GET",
            path: "/screenshot/annotated".into(),
            body: None,
        }),
        "propose_intent" => {
            let utterance = required_str(args, "utterance")?;
            Ok(BridgeCall {
                method: "POST",
                path: "/intent".into(),
                body: Some(json!({ "utterance": utterance }).to_string()),
            })
        }
        "confirm_intent" => Ok(BridgeCall {
            method: "POST",
            path: "/intent/confirm".into(),
            body: None,
        }),
        other => Err(format!("unknown tool '{other}'")),
    }
}

pub fn tool_list() -> Value {
    json!({
        "tools": [
            tool("observe", "Propose a read of the accessibility summary. Nothing is read until the window confirms.", json!({})),
            tool("a11y_index", "Propose a read of the numbered accessibility index. Nothing is read until the window confirms.", json!({})),
            tool("query", "Propose a DOM selector query. Nothing is read until the window confirms.", json!({"type":"object","properties":{"selector":{"type":"string"}},"required":["selector"]})),
            tool("click", "Propose a click by CSS selector or a11y index. Nothing is clicked until the window confirms.", json!({"type":"object","properties":{"selector":{"type":"string"},"index":{"type":"integer"}}})),
            tool("type", "Propose typing into a selector or a11y index. Nothing is typed until the window confirms.", json!({"type":"object","properties":{"selector":{"type":"string"},"index":{"type":"integer"},"text":{"type":"string"}},"required":["text"]})),
            tool("screenshot", "Propose a PNG capture. Nothing is captured until the window confirms.", json!({})),
            tool("screenshot_annotated", "Propose an annotated capture. Nothing is captured until the window confirms.", json!({})),
            tool("propose_intent", "Propose an L0 shell intent. The window confirms before it runs.", json!({"type":"object","properties":{"utterance":{"type":"string"}},"required":["utterance"]})),
        ]
    })
}

/// Handle one JSON-RPC message. `None` means the client sent a notification.
///
/// `tools/call` does not call `exec`. `w3cos/result` reads the stored outcome.
/// There is no confirm method.
pub fn dispatch(
    message: &Value,
    exec: &mut dyn FnMut(&BridgeCall) -> (u16, String),
) -> Option<Value> {
    let id = message.get("id").cloned();
    id.as_ref()?;
    let method = message
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "w3cos-ai-mcp", "version": "0.1.0" }
        })),
        "tools/list" => Ok(tool_list()),
        "tools/call" => match describe_call(&params) {
            Ok(text) => Ok(content_body(&text, false)),
            Err(message) => Ok(content_body(&message, true)),
        },
        "w3cos/result" => {
            let (status, body) = exec(&result_call());
            let is_error = !(200..300).contains(&status);
            Ok(content_body(&format!("HTTP {status}\n{body}"), is_error))
        }
        "ping" => Ok(json!({})),
        other => Err(format!("unknown method '{other}'")),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": message }
        }),
    })
}

fn describe_call(params: &Value) -> Result<String, String> {
    let name = params
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| say("mcp-unknown", &[("name", "")]))?;
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let _validated = accept_tool(name, &arguments)?;
    Ok(say("mcp-awaiting", &[("name", name)]))
}

fn content_body(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

/// stdio `tools/call`: post `/mcp/propose`, then poll `/mcp/result`.
/// The tool's own bridge request is never passed to `http`.
pub fn drive_tool_call(
    params: &Value,
    http: &mut dyn FnMut(&BridgeCall) -> (u16, String),
    pause: &mut dyn FnMut(),
    max_polls: u32,
) -> Result<String, String> {
    let name = params
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    accept_tool(name, &arguments)?;
    let propose = BridgeCall {
        method: "POST",
        path: "/mcp/propose".into(),
        body: Some(json!({ "name": name, "arguments": arguments }).to_string()),
    };
    let (status, body) = http(&propose);
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}\n{body}"));
    }
    let mut polls = max_polls.max(1);
    loop {
        let (status, body) = http(&result_call());
        if result_ready(&body) {
            if (200..300).contains(&status) && outcome_ok(&body) {
                return Ok(format!("HTTP {status}\n{body}"));
            }
            return Err(format!("HTTP {status}\n{body}"));
        }
        polls = polls.saturating_sub(1);
        if polls == 0 {
            return Ok(say("mcp-awaiting", &[("name", name)]));
        }
        pause();
    }
}

fn result_call() -> BridgeCall {
    BridgeCall {
        method: "GET",
        path: "/mcp/result".into(),
        body: None,
    }
}

fn result_ready(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    value.get("pending").and_then(|item| item.as_bool()) == Some(false)
        && value.get("status").is_some()
}

fn outcome_ok(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("status").and_then(|item| item.as_u64()))
        .is_some_and(|status| (200..300).contains(&status))
}

/// One proposal the shell can arm. The bridge call is stored and not run.
#[derive(Clone, Debug)]
pub struct ProposeView {
    pub name: String,
    pub summary: String,
    pub impact: String,
}

#[derive(Clone, Debug)]
pub struct ProposeError {
    pub pending: bool,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct ToolOutcome {
    pub name: String,
    pub summary: String,
    pub status: u16,
    pub note: String,
}

struct PendingTool {
    name: String,
    summary: String,
    call: BridgeCall,
    armed: bool,
}

struct FinishedTool {
    name: String,
    summary: String,
    status: u16,
    note: String,
    /// Client-facing body. The receipt keeps the shorter `note`.
    body: String,
    recorded: bool,
}

#[derive(Default)]
struct Gate {
    pending: Option<PendingTool>,
    inflight: Option<(String, String)>,
    done: VecDeque<FinishedTool>,
}

fn gate() -> &'static Mutex<Gate> {
    static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(Gate::default()))
}

fn lock_gate() -> std::sync::MutexGuard<'static, Gate> {
    gate().lock().unwrap_or_else(|err| err.into_inner())
}

pub fn reset_gate() {
    *lock_gate() = Gate::default();
}

pub fn submit_propose(body: &str) -> Result<ProposeView, ProposeError> {
    let value: Value = serde_json::from_str(body).map_err(|_| ProposeError {
        pending: false,
        message: "invalid json".into(),
    })?;
    let name = value
        .get("name")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let arguments = value.get("arguments").cloned().unwrap_or(Value::Null);
    propose(name, &arguments)
}

/// Store one bridge call. Does not run it. A second call is refused.
pub fn propose_call(name: &str, call: BridgeCall) -> Result<ProposeView, ProposeError> {
    if name.is_empty() || name == "confirm_intent" {
        return Err(ProposeError {
            pending: false,
            message: say("mcp-not-a-tool", &[]),
        });
    }
    let mut gate = lock_gate();
    if gate.pending.is_some() {
        return Err(ProposeError {
            pending: true,
            message: say("mcp-pending", &[]),
        });
    }
    let summary = say("mcp-tool-summary", &[("name", name)]);
    let view = ProposeView {
        name: name.to_string(),
        summary,
        impact: say("mcp-tool-impact", &[]),
    };
    gate.pending = Some(PendingTool {
        name: view.name.clone(),
        summary: view.summary.clone(),
        call,
        armed: false,
    });
    Ok(view)
}

pub fn propose(name: &str, arguments: &Value) -> Result<ProposeView, ProposeError> {
    let accepted = accept_tool(name, arguments).map_err(|message| ProposeError {
        pending: false,
        message,
    })?;
    let mut gate = lock_gate();
    if gate.pending.is_some() {
        return Err(ProposeError {
            pending: true,
            message: say("mcp-pending", &[]),
        });
    }
    let view = ProposeView {
        name: accepted.name,
        summary: accepted.summary,
        impact: accepted.impact,
    };
    gate.pending = Some(PendingTool {
        name: view.name.clone(),
        summary: view.summary.clone(),
        call: accepted.call,
        armed: false,
    });
    Ok(view)
}

/// Mark the stored call ready. Does not execute it. A read or screenshot asks
/// the UI thread to copy layout, and pixels, before the next poll runs it.
pub fn arm() -> bool {
    let prep = {
        let mut gate = lock_gate();
        let Some(pending) = gate.pending.as_mut() else {
            return false;
        };
        pending.armed = true;
        (pending.call.method, pending.call.path.clone())
    };
    match prep {
        ("GET", path) if path == "/screenshot" || path == "/screenshot/annotated" => {
            crate::server::note_bridge_prep(true, true);
        }
        ("GET", path)
            if path == "/a11y"
                || path == "/a11y/index"
                || path == "/a11y/json"
                || path.starts_with("/query") =>
        {
            crate::server::note_bridge_prep(true, false);
        }
        _ => {}
    }
    true
}

/// Take a call the window has armed. `None` while it is still waiting.
pub fn take_armed() -> Option<BridgeCall> {
    let mut gate = lock_gate();
    let armed = gate.pending.as_ref().is_some_and(|pending| pending.armed);
    if !armed {
        return None;
    }
    let pending = gate.pending.take()?;
    gate.inflight = Some((pending.name, pending.summary));
    Some(pending.call)
}

/// Drop the stored call. The returned name is what the shell writes on the
/// receipt. A later poll sees the cancellation and does not run the call.
pub fn cancel() -> Option<String> {
    let mut gate = lock_gate();
    let pending = gate.pending.take()?;
    let note = say("tool-cancelled-notice", &[]);
    gate.done.push_back(FinishedTool {
        name: pending.name.clone(),
        summary: pending.summary,
        status: 499,
        body: note.clone(),
        note,
        recorded: true,
    });
    Some(pending.name)
}

pub fn store_bridge_response(status: u16, note: &str) {
    store_bridge_outcome(status, note, note);
}

/// `note` is what the ledger keeps. `body` is what `/mcp/result` returns.
pub fn store_bridge_outcome(status: u16, note: &str, body: &str) {
    let mut gate = lock_gate();
    let (name, summary) = gate
        .inflight
        .take()
        .unwrap_or_else(|| ("tool".into(), String::new()));
    let note: String = note.chars().take(240).collect();
    let body = if body.len() > 256 * 1024 {
        format!("HTTP {status} · {} bytes", body.len())
    } else {
        body.to_string()
    };
    gate.done.push_back(FinishedTool {
        name,
        summary,
        status,
        note,
        body,
        recorded: false,
    });
    while gate.done.len() > 8 {
        gate.done.pop_front();
    }
}

pub fn take_done() -> Option<ToolOutcome> {
    let mut gate = lock_gate();
    let position = gate.done.iter().position(|item| !item.recorded)?;
    let done = &mut gate.done[position];
    done.recorded = true;
    Some(ToolOutcome {
        name: done.name.clone(),
        summary: done.summary.clone(),
        status: done.status,
        note: done.note.clone(),
    })
}

pub fn result_json() -> Value {
    let gate = lock_gate();
    if let Some(pending) = &gate.pending {
        return json!({
            "pending": true,
            "armed": pending.armed,
            "name": pending.name,
            "summary": pending.summary
        });
    }
    if let Some(done) = gate.done.back() {
        let body = if done.body.is_empty() {
            done.note.as_str()
        } else {
            done.body.as_str()
        };
        return json!({
            "pending": false,
            "name": done.name,
            "status": done.status,
            "body": body
        });
    }
    json!({
        "pending": false,
        "summary": say("mcp-nothing", &[])
    })
}

struct AcceptedTool {
    name: String,
    summary: String,
    impact: String,
    call: BridgeCall,
}

fn accept_tool(name: &str, arguments: &Value) -> Result<AcceptedTool, String> {
    if name == "confirm_intent" {
        return Err(say("mcp-not-a-tool", &[]));
    }
    let call = tool_to_bridge(name, arguments).map_err(|err| {
        if err.starts_with("unknown tool") {
            say("mcp-unknown", &[("name", name)])
        } else {
            err
        }
    })?;
    Ok(AcceptedTool {
        name: name.to_string(),
        summary: say("mcp-tool-summary", &[("name", name)]),
        impact: say("mcp-tool-impact", &[]),
        call,
    })
}

fn say(key: &str, args: &[(&str, &str)]) -> String {
    crate::copy::line(&locale_tag(), key, args)
}

fn locale_tag() -> String {
    std::env::var("W3COS_LOCALE").unwrap_or_else(|_| "zh-CN".into())
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema
    })
}

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("missing '{key}'"))
}

fn target_body(args: &Value) -> Result<String, String> {
    let selector = args
        .get("selector")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    let index = args.get("index").and_then(|value| value.as_u64());
    match (selector.is_empty(), index) {
        (false, Some(_)) => Err("provide 'selector' or 'index', not both".into()),
        (true, None) => Err("missing 'selector' or 'index'".into()),
        (false, None) => Ok(json!({ "selector": selector }).to_string()),
        (true, Some(index)) => Ok(json!({ "index": index }).to_string()),
    }
}

pub(crate) fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One loopback request. The bridge, not this function, checks the token.
pub fn http_bridge(port: u16, token: &str, call: &BridgeCall) -> Result<(u16, String), String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| err.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let body = call.body.clone().unwrap_or_default();
    let auth = if token.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {token}\r\n")
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        method = call.method,
        path = call.path,
        len = body.len(),
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| err.to_string())?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap_or(0);
    Ok((status, body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_map_onto_the_existing_bridge() {
        let index = tool_to_bridge("a11y_index", &Value::Null).unwrap();
        assert_eq!(index.method, "GET");
        assert_eq!(index.path, "/a11y/index");
        let propose = tool_to_bridge("propose_intent", &json!({"utterance": "打开终端"})).unwrap();
        assert_eq!(propose.path, "/intent");
        assert!(propose.body.unwrap().contains("打开终端"));
        let click = tool_to_bridge("click", &json!({"index": 2})).unwrap();
        assert_eq!(click.body.as_deref(), Some("{\"index\":2}"));
        assert!(tool_to_bridge("click", &json!({"selector":"#go","index":1})).is_err());
    }

    #[test]
    fn tools_call_does_not_execute_and_confirm_is_not_a_method() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "a11y_index", "arguments": {} }
        });
        let response = dispatch(&message, &mut |_| panic!("tools/call must not execute")).unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("a11y_index"), "{text}");
        assert_eq!(response["result"]["isError"], false);

        let confirm = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": { "name": "confirm_intent", "arguments": {} }
        });
        let response =
            dispatch(&confirm, &mut |_| panic!("confirm_intent must not execute")).unwrap();
        assert_eq!(response["result"]["isError"], true);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("确认") || text.contains("Confirm"), "{text}");

        let method = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "w3cos/confirm"
        });
        let response =
            dispatch(&method, &mut |_| panic!("w3cos/confirm must not execute")).unwrap();
        assert!(response.get("error").is_some(), "{response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("w3cos/confirm")
        );

        let names = tool_list()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"a11y_index".to_string()));
        assert!(names.contains(&"propose_intent".to_string()));
        assert!(!names.iter().any(|name| name == "confirm_intent"));
    }

    #[test]
    fn propose_stores_one_call_and_confirm_runs_it_once() {
        reset_gate();
        let first = propose("a11y_index", &Value::Null).unwrap();
        assert!(first.summary.contains("a11y_index"));
        let second = propose("observe", &Value::Null).unwrap_err();
        assert!(second.pending);
        assert!(take_armed().is_none());

        assert!(arm());
        let mut runs = 0;
        let call = take_armed().expect("armed call");
        assert_eq!(call.method, "GET");
        assert_eq!(call.path, "/a11y/index");
        runs += 1;
        store_bridge_response(200, "[{\"index\":1}]");
        assert!(take_armed().is_none());
        assert_eq!(runs, 1);
        let done = take_done().unwrap();
        assert_eq!(done.name, "a11y_index");
        assert_eq!(done.status, 200);
        assert!(done.note.contains("index"));
        assert!(take_done().is_none());

        propose("click", &json!({"index": 3})).unwrap();
        assert_eq!(cancel().as_deref(), Some("click"));
        assert!(take_armed().is_none());
        let body = result_json();
        assert_eq!(body["pending"], false);
        assert_eq!(body["status"], 499);
        assert!(take_done().is_none());
    }

    #[test]
    fn stdio_propose_polls_the_result_and_skips_the_tool_request() {
        let params = json!({"name": "a11y_index", "arguments": {}});
        let mut paths = Vec::new();
        let mut polls = 0;
        let text = drive_tool_call(
            &params,
            &mut |call| {
                paths.push(call.path.clone());
                if call.path == "/mcp/propose" {
                    assert_eq!(call.method, "POST");
                    (200, "{\"pending\":true}".into())
                } else {
                    polls += 1;
                    if polls < 2 {
                        (200, "{\"pending\":true,\"name\":\"a11y_index\"}".into())
                    } else {
                        (
                            200,
                            "{\"pending\":false,\"status\":200,\"body\":\"[{\\\"index\\\":1}]\"}"
                                .into(),
                        )
                    }
                }
            },
            &mut || {},
            4,
        )
        .unwrap();
        assert_eq!(paths, vec!["/mcp/propose", "/mcp/result", "/mcp/result"]);
        assert!(text.contains("200"), "{text}");

        let mut called = false;
        let err = drive_tool_call(
            &json!({"name": "confirm_intent", "arguments": {}}),
            &mut |_| {
                called = true;
                (200, String::new())
            },
            &mut || {},
            1,
        )
        .unwrap_err();
        assert!(!called);
        assert!(err.contains("确认") || err.contains("Confirm"), "{err}");
    }
}
