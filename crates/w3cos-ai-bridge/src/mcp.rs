//! Minimal MCP adapter over the existing AI Bridge HTTP API.
//!
//! This is not a second DOM or permission model. Tool calls become the same
//! loopback requests the bridge already authorizes. An observer token can
//! read the a11y index. Propose, confirm, click, and type need interaction
//! permission, which the bridge enforces.

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
            tool("observe", "Read the accessibility summary. Observer tokens may call this.", json!({})),
            tool("a11y_index", "Read the numbered accessibility index without a screenshot.", json!({})),
            tool("query", "Query one DOM selector.", json!({"type":"object","properties":{"selector":{"type":"string"}},"required":["selector"]})),
            tool("click", "Click by CSS selector or a11y index. Requires interaction permission.", json!({"type":"object","properties":{"selector":{"type":"string"},"index":{"type":"integer"}}})),
            tool("type", "Type into a selector or a11y index. Requires write permission.", json!({"type":"object","properties":{"selector":{"type":"string"},"index":{"type":"integer"},"text":{"type":"string"}},"required":["text"]})),
            tool("screenshot", "Capture the raw PNG. Requires screenshot permission.", json!({})),
            tool("screenshot_annotated", "Capture a PNG plus the layout index map. Requires screenshot permission.", json!({})),
            tool("propose_intent", "Propose an L0 shell intent. Requires interaction permission. Does not execute it.", json!({"type":"object","properties":{"utterance":{"type":"string"}},"required":["utterance"]})),
            tool("confirm_intent", "Confirm the pending L0 intent. Requires interaction permission.", json!({})),
        ]
    })
}

/// Handle one JSON-RPC message. `None` means the client sent a notification.
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
        "tools/call" => match call_tool(&params, exec) {
            Ok(body) => Ok(body),
            Err(message) => Ok(json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true
            })),
        },
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

fn call_tool(
    params: &Value,
    exec: &mut dyn FnMut(&BridgeCall) -> (u16, String),
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or("missing tool name")?;
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let call = tool_to_bridge(name, &arguments)?;
    let (status, body) = exec(&call);
    let is_error = !(200..300).contains(&status);
    Ok(json!({
        "content": [{ "type": "text", "text": format!("HTTP {status}\n{body}") }],
        "isError": is_error
    }))
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

fn url_encode(text: &str) -> String {
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
    fn dispatch_reports_the_bridge_status() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "a11y_index", "arguments": {} }
        });
        let response = dispatch(&message, &mut |call| {
            assert_eq!(call.path, "/a11y/index");
            (200, "[{\"index\":1}]".into())
        })
        .unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("HTTP 200"));
        assert_eq!(response["result"]["isError"], false);

        let denied = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": { "name": "propose_intent", "arguments": { "utterance": "open files" } }
        });
        let response = dispatch(&denied, &mut |_| {
            (403, "{\"error\":\"no interact\"}".into())
        })
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
        let names = tool_list()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"a11y_index".to_string()));
        assert!(names.contains(&"propose_intent".to_string()));
    }
}
