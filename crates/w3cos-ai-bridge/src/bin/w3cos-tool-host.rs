//! stdio MCP adapter for the local device and wallet host.
//!
//! ```text
//! W3COS_AI_PORT=9222 W3COS_AI_TOKEN=... w3cos-tool-host
//! ```
//!
//! `tools/call` posts `/host/propose` and polls `/host/result`. The process
//! that owns the window is the only caller of confirm. This binary has no
//! confirm method and does not hold the device seed.

use std::io::{self, Read, Write};

use serde_json::Value;
use w3cos_ai_bridge::mcp::{self, BridgeCall};
use w3cos_ai_bridge::tool_host;

fn main() {
    let port = std::env::var("W3COS_AI_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(9222u16);
    let token = std::env::var("W3COS_AI_TOKEN").unwrap_or_default();
    if token.is_empty() {
        eprintln!("[w3cos-tool-host] W3COS_AI_TOKEN is empty; the bridge will reject calls");
    }
    eprintln!("[w3cos-tool-host] forwarding to http://127.0.0.1:{port}");
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::stdout().lock();
    while let Some(message) = read_message(&mut input) {
        let Ok(message) = serde_json::from_str::<Value>(&message) else {
            continue;
        };
        let Some(response) = tool_host::stdio_respond(
            &message,
            &mut |call: &BridgeCall| match mcp::http_bridge(port, &token, call) {
                Ok(result) => result,
                Err(err) => (502, err),
            },
            &mut || std::thread::sleep(std::time::Duration::from_millis(200)),
            150,
        ) else {
            continue;
        };
        if write_message(&mut stdout, &response.to_string()).is_err() {
            break;
        }
    }
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
            let mut line = vec![b'{'];
            let mut buf = [0u8; 1];
            loop {
                if input.read(&mut buf).ok()? == 0 {
                    break;
                }
                if buf[0] == b'\n' {
                    break;
                }
                line.push(buf[0]);
            }
            return Some(String::from_utf8_lossy(&line).into_owned());
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
