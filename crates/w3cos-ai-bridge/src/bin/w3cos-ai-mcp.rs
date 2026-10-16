//! stdio MCP adapter for a running W3C OS AI Bridge.
//!
//! ```text
//! W3COS_AI_PORT=9222 W3COS_AI_TOKEN=... w3cos-ai-mcp
//! ```
//!
//! The process speaks MCP `initialize`, `tools/list`, `tools/call`, and
//! `w3cos/result` on stdin. `tools/call` posts `/mcp/propose` and polls
//! `/mcp/result`. It does not send the tool's own bridge request. The window
//! is the only place that confirms. There is no `w3cos/confirm` method.

use std::io::{self, Read, Write};

use serde_json::Value;
use w3cos_ai_bridge::mcp::{self, BridgeCall};

fn main() {
    let port = std::env::var("W3COS_AI_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(9222u16);
    let token = std::env::var("W3COS_AI_TOKEN").unwrap_or_default();
    if token.is_empty() {
        eprintln!("[w3cos-ai-mcp] W3COS_AI_TOKEN is empty; the bridge will reject calls");
    }
    eprintln!("[w3cos-ai-mcp] forwarding to http://127.0.0.1:{port}");
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::stdout().lock();
    while let Some(message) = read_message(&mut input) {
        let Ok(message) = serde_json::from_str::<Value>(&message) else {
            continue;
        };
        let Some(response) = respond(&message, port, &token) else {
            continue;
        };
        if write_message(&mut stdout, &response.to_string()).is_err() {
            break;
        }
    }
}

fn bridge(port: u16, token: &str, call: &BridgeCall) -> (u16, String) {
    match mcp::http_bridge(port, token, call) {
        Ok(result) => result,
        Err(err) => (502, err),
    }
}

fn respond(message: &Value, port: u16, token: &str) -> Option<Value> {
    let method = message
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if method == "tools/call" {
        let id = message.get("id").cloned()?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match mcp::drive_tool_call(
            &params,
            &mut |call| bridge(port, token, call),
            &mut pause_for_confirm,
            150,
        ) {
            Ok(text) => mcp_content(&text, false),
            Err(text) => mcp_content(&text, true),
        };
        return Some(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
    mcp::dispatch(message, &mut |call| bridge(port, token, call))
}

fn pause_for_confirm() {
    std::thread::sleep(std::time::Duration::from_millis(200));
}

fn mcp_content(text: &str, is_error: bool) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

fn read_message(input: &mut impl Read) -> Option<String> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if input.read(&mut byte).ok()? == 0 {
            return None;
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header == b"{" || (header.last() == Some(&b'{') && !header.contains(&b'\n')) {
            // Newline-delimited JSON. Keep the '{'.
            let mut rest = String::new();
            let mut buf = [0u8; 1];
            let mut line = vec![b'{'];
            loop {
                if input.read(&mut buf).ok()? == 0 {
                    break;
                }
                if buf[0] == b'\n' {
                    break;
                }
                line.push(buf[0]);
            }
            rest.push_str(&String::from_utf8_lossy(&line));
            return Some(rest);
        }
        if header.len() > 16 * 1024 {
            return None;
        }
    }
    let header_text = String::from_utf8_lossy(&header);
    let length = header_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    })?;
    let mut body = vec![0u8; length];
    input.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}

fn write_message(output: &mut impl Write, body: &str) -> io::Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    output.flush()
}
