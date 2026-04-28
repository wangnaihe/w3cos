use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use crate::a11y_api;
use crate::dom_access::{self, ActionType, DomAction};
use w3cos_dom::document::Document;

/// Request from the HTTP server thread to the main event loop.
pub enum AiBridgeRequest {
    GetA11yTree,
    GetA11yJson,
    Screenshot,
    Click { selector: String },
    Type { selector: String, text: String },
    Query { selector: String },
}

/// Source of frame snapshots — supplied by the runtime so the bridge can
/// stay decoupled from the render pipeline.
pub trait ScreenshotProvider: Send + Sync {
    /// Return the latest framebuffer encoded as PNG. `None` when no frame
    /// has been rendered yet (or the provider can't capture in this mode).
    fn capture_png(&self) -> Option<Vec<u8>>;
}

/// Response from the main event loop back to the HTTP server thread.
pub enum AiBridgeResponse {
    Text(String),
    Json(String),
    Png(Vec<u8>),
    Error(String),
}

/// Handle held by the main event loop to poll and respond to AI bridge requests.
pub struct AiBridgeHandle {
    rx: mpsc::Receiver<(AiBridgeRequest, mpsc::Sender<AiBridgeResponse>)>,
    screenshot_provider: std::sync::Arc<dyn ScreenshotProvider>,
}

impl AiBridgeHandle {
    /// Replace the screenshot provider after construction.
    pub fn set_screenshot_provider(&mut self, provider: std::sync::Arc<dyn ScreenshotProvider>) {
        self.screenshot_provider = provider;
    }

    /// Poll for pending requests from the HTTP server.
    /// Called from the main event loop (same thread as Document).
    pub fn poll_and_respond(&self, doc: &mut Document) {
        while let Ok((request, reply_tx)) = self.rx.try_recv() {
            let response = self.handle_request(request, doc);
            let _ = reply_tx.send(response);
        }
    }

    fn handle_request(&self, request: AiBridgeRequest, doc: &mut Document) -> AiBridgeResponse {
        match request {
            AiBridgeRequest::GetA11yTree => {
                let summary = a11y_api::get_ui_summary(doc);
                AiBridgeResponse::Text(summary)
            }
            AiBridgeRequest::GetA11yJson => {
                let json = a11y_api::get_tree_json(doc);
                AiBridgeResponse::Json(json)
            }
            AiBridgeRequest::Screenshot => match self.screenshot_provider.capture_png() {
                Some(png) => AiBridgeResponse::Png(png),
                None => AiBridgeResponse::Json(
                    r#"{"error":"no frame captured yet — render at least one frame first or enable CPU rendering"}"#
                        .to_string(),
                ),
            },
            AiBridgeRequest::Click { selector } => {
                let action = DomAction {
                    action: ActionType::Click,
                    selector,
                    value: None,
                };
                let result = dom_access::execute(doc, &action);
                let json = serde_json::to_string(&result).unwrap_or_default();
                AiBridgeResponse::Json(json)
            }
            AiBridgeRequest::Type { selector, text } => {
                let action = DomAction {
                    action: ActionType::SetText,
                    selector,
                    value: Some(text),
                };
                let result = dom_access::execute(doc, &action);
                let json = serde_json::to_string(&result).unwrap_or_default();
                AiBridgeResponse::Json(json)
            }
            AiBridgeRequest::Query { selector } => {
                let result = dom_access::query(doc, &selector);
                let json = serde_json::to_string(&result).unwrap_or_default();
                AiBridgeResponse::Json(json)
            }
        }
    }
}

/// Default no-op screenshot provider used until the runtime supplies a real one.
struct NullScreenshotProvider;

impl ScreenshotProvider for NullScreenshotProvider {
    fn capture_png(&self) -> Option<Vec<u8>> {
        None
    }
}

/// Start the AI Bridge HTTP server on the given port.
/// Returns a handle for the main event loop to poll.
pub fn start(port: u16) -> AiBridgeHandle {
    start_with_provider(port, std::sync::Arc::new(NullScreenshotProvider))
}

/// Start the AI Bridge HTTP server with a custom screenshot provider.
pub fn start_with_provider(
    port: u16,
    provider: std::sync::Arc<dyn ScreenshotProvider>,
) -> AiBridgeHandle {
    let (tx, rx) = mpsc::channel::<(AiBridgeRequest, mpsc::Sender<AiBridgeResponse>)>();

    thread::spawn(move || {
        let addr = format!("127.0.0.1:{port}");
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => {
                eprintln!("[AI Bridge] HTTP server listening on http://{addr}");
                l
            }
            Err(e) => {
                eprintln!("[AI Bridge] Failed to bind {addr}: {e}");
                return;
            }
        };

        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            if let Err(e) = handle_connection(stream, &tx) {
                eprintln!("[AI Bridge] Connection error: {e}");
            }
        }
    });

    AiBridgeHandle {
        rx,
        screenshot_provider: provider,
    }
}

fn handle_connection(
    mut stream: TcpStream,
    tx: &mpsc::Sender<(AiBridgeRequest, mpsc::Sender<AiBridgeResponse>)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        headers.push(line);
    }

    let content_length = headers
        .iter()
        .find_map(|h| {
            let lower = h.to_lowercase();
            if lower.starts_with("content-length:") {
                lower.split(':').nth(1)?.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    let body_str = String::from_utf8_lossy(&body).to_string();

    let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
    if parts.len() < 2 {
        send_response(&mut stream, 400, "text/plain", b"Bad Request")?;
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    let request = match (method, path) {
        ("GET", "/a11y") => Some(AiBridgeRequest::GetA11yTree),
        ("GET", "/a11y/json") => Some(AiBridgeRequest::GetA11yJson),
        ("GET", "/screenshot") => Some(AiBridgeRequest::Screenshot),
        ("POST", "/click") => {
            let body: serde_json::Value =
                serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null);
            let selector = body["selector"]
                .as_str()
                .unwrap_or("")
                .to_string();
            if selector.is_empty() {
                send_response(
                    &mut stream,
                    400,
                    "application/json",
                    br#"{"error":"missing 'selector' field"}"#,
                )?;
                return Ok(());
            }
            Some(AiBridgeRequest::Click { selector })
        }
        ("POST", "/type") => {
            let body: serde_json::Value =
                serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null);
            let selector = body["selector"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let text = body["text"]
                .as_str()
                .unwrap_or("")
                .to_string();
            if selector.is_empty() {
                send_response(
                    &mut stream,
                    400,
                    "application/json",
                    br#"{"error":"missing 'selector' field"}"#,
                )?;
                return Ok(());
            }
            Some(AiBridgeRequest::Type { selector, text })
        }
        ("GET", "/query") => {
            let selector = path
                .split('?')
                .nth(1)
                .and_then(|qs| {
                    qs.split('&').find_map(|param| {
                        let mut parts = param.splitn(2, '=');
                        if parts.next()? == "selector" {
                            Some(urlish_decode(parts.next()?))
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_default();
            if selector.is_empty() {
                send_response(
                    &mut stream,
                    400,
                    "application/json",
                    br#"{"error":"missing 'selector' query param"}"#,
                )?;
                return Ok(());
            }
            Some(AiBridgeRequest::Query { selector })
        }
        ("GET", "/") | ("GET", "/health") => {
            let info = serde_json::json!({
                "service": "W3C OS AI Bridge",
                "version": "0.1.0",
                "endpoints": [
                    {"method": "GET", "path": "/a11y", "description": "Accessibility tree (text summary)"},
                    {"method": "GET", "path": "/a11y/json", "description": "Accessibility tree (full JSON)"},
                    {"method": "GET", "path": "/screenshot", "description": "PNG screenshot"},
                    {"method": "POST", "path": "/click", "description": "Click element by selector"},
                    {"method": "POST", "path": "/type", "description": "Type text into element"},
                    {"method": "GET", "path": "/query?selector=...", "description": "Query element info"},
                ]
            });
            let body = serde_json::to_string_pretty(&info)?;
            send_response(&mut stream, 200, "application/json", body.as_bytes())?;
            return Ok(());
        }
        _ => {
            send_response(&mut stream, 404, "text/plain", b"Not Found")?;
            return Ok(());
        }
    };

    if let Some(req) = request {
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send((req, reply_tx))?;

        match reply_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(response) => match response {
                AiBridgeResponse::Text(text) => {
                    send_response(&mut stream, 200, "text/plain; charset=utf-8", text.as_bytes())?;
                }
                AiBridgeResponse::Json(json) => {
                    send_response(&mut stream, 200, "application/json", json.as_bytes())?;
                }
                AiBridgeResponse::Png(data) => {
                    send_response(&mut stream, 200, "image/png", &data)?;
                }
                AiBridgeResponse::Error(msg) => {
                    let err = serde_json::json!({"error": msg});
                    send_response(
                        &mut stream,
                        500,
                        "application/json",
                        err.to_string().as_bytes(),
                    )?;
                }
            },
            Err(_) => {
                send_response(
                    &mut stream,
                    504,
                    "application/json",
                    br#"{"error":"request timed out"}"#,
                )?;
            }
        }
    }

    Ok(())
}

fn send_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), std::io::Error> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        504 => "Gateway Timeout",
        _ => "Unknown",
    };
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn urlish_decode(s: &str) -> String {
    s.replace("%20", " ")
        .replace("%23", "#")
        .replace("%2E", ".")
        .replace("%2F", "/")
        .replace("%3A", ":")
        .replace("%3D", "=")
        .replace("%26", "&")
        .replace('+', " ")
}
