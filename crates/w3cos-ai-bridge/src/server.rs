use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::a11y_api;
use crate::dom_access::{self, ActionType, DomAction};
use crate::permissions::{self, ActionRateLimiter, AgentPermissions, BridgeOp};
use w3cos_dom::document::Document;

/// Largest request body the bridge will buffer. Larger `Content-Length` values
/// are rejected before allocation.
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_HEADERS: usize = 64;

/// Request from the HTTP server thread to the main event loop.
pub enum AiBridgeRequest {
    GetA11yTree,
    GetA11yJson,
    Screenshot,
    Click {
        selector: String,
    },
    Type {
        selector: String,
        text: String,
    },
    Query {
        selector: String,
    },
    Act {
        action: ActionType,
        selector: String,
        value: Option<String>,
    },
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
    Denied(String),
    RateLimited(String),
}

/// Handle held by the main event loop to poll and respond to AI bridge requests.
pub struct AiBridgeHandle {
    rx: mpsc::Receiver<(AiBridgeRequest, mpsc::Sender<AiBridgeResponse>)>,
    screenshot_provider: std::sync::Arc<dyn ScreenshotProvider>,
    permissions: AgentPermissions,
    rate: RefCell<ActionRateLimiter>,
}

impl AiBridgeHandle {
    /// Replace the screenshot provider after construction.
    pub fn set_screenshot_provider(&mut self, provider: std::sync::Arc<dyn ScreenshotProvider>) {
        self.screenshot_provider = provider;
    }

    pub fn permissions(&self) -> &AgentPermissions {
        &self.permissions
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
        let op = match &request {
            AiBridgeRequest::GetA11yTree | AiBridgeRequest::GetA11yJson => BridgeOp::ReadDom,
            AiBridgeRequest::Screenshot => BridgeOp::Screenshot,
            AiBridgeRequest::Query { selector } => BridgeOp::Query {
                selector: selector.clone(),
            },
            AiBridgeRequest::Click { selector } => BridgeOp::Mutate {
                action: ActionType::Click,
                selector: selector.clone(),
            },
            AiBridgeRequest::Type { selector, .. } => BridgeOp::Mutate {
                action: ActionType::SetText,
                selector: selector.clone(),
            },
            AiBridgeRequest::Act {
                action, selector, ..
            } => BridgeOp::Mutate {
                action: *action,
                selector: selector.clone(),
            },
        };
        if let Err(err) = permissions::authorize(&self.permissions, &op) {
            return AiBridgeResponse::Denied(err.message("bridge"));
        }
        if op.is_mutating()
            && !self
                .rate
                .borrow_mut()
                .try_acquire(now_ms(), self.permissions.max_actions_per_second)
        {
            return AiBridgeResponse::RateLimited(
                "Agent 'bridge' exceeded max_actions_per_second".to_string(),
            );
        }
        match request {
            AiBridgeRequest::GetA11yTree => {
                AiBridgeResponse::Text(a11y_api::get_ui_summary(doc))
            }
            AiBridgeRequest::GetA11yJson => AiBridgeResponse::Json(a11y_api::get_tree_json(doc)),
            AiBridgeRequest::Screenshot => match self.screenshot_provider.capture_png() {
                Some(png) => AiBridgeResponse::Png(png),
                None => AiBridgeResponse::Json(
                    r#"{"error":"no frame captured yet — render at least one frame first or enable CPU rendering"}"#
                        .to_string(),
                ),
            },
            AiBridgeRequest::Click { selector } => {
                mutate(doc, ActionType::Click, selector, None)
            }
            AiBridgeRequest::Type { selector, text } => {
                mutate(doc, ActionType::SetText, selector, Some(text))
            }
            AiBridgeRequest::Query { selector } => {
                let result = dom_access::query(doc, &selector);
                AiBridgeResponse::Json(serde_json::to_string(&result).unwrap_or_default())
            }
            AiBridgeRequest::Act {
                action,
                selector,
                value,
            } => mutate(doc, action, selector, value),
        }
    }
}

fn mutate(
    doc: &mut Document,
    action: ActionType,
    selector: String,
    value: Option<String>,
) -> AiBridgeResponse {
    let action = DomAction {
        action,
        selector,
        value,
    };
    let result = dom_access::execute(doc, &action);
    AiBridgeResponse::Json(serde_json::to_string(&result).unwrap_or_default())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Default no-op screenshot provider used until the runtime supplies a real one.
struct NullScreenshotProvider;

impl ScreenshotProvider for NullScreenshotProvider {
    fn capture_png(&self) -> Option<Vec<u8>> {
        None
    }
}

/// Options for [`start_with_options`]. `token: None` reads `W3COS_AI_TOKEN`
/// or generates one. The listener always binds `127.0.0.1`.
pub struct BridgeOptions {
    pub port: u16,
    pub permissions: AgentPermissions,
    pub token: Option<String>,
    pub screenshot: Option<std::sync::Arc<dyn ScreenshotProvider>>,
}

pub struct RunningBridge {
    pub handle: AiBridgeHandle,
    pub addr: SocketAddr,
    pub token: String,
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
    start_with_options(BridgeOptions {
        port,
        permissions: permissions_from_env(),
        token: None,
        screenshot: Some(provider),
    })
    .handle
}

/// Bind the loopback listener and return the address plus the bearer token.
pub fn start_with_options(options: BridgeOptions) -> RunningBridge {
    let token = resolve_token(options.token);
    let permissions = options.permissions;
    let provider = options
        .screenshot
        .unwrap_or_else(|| std::sync::Arc::new(NullScreenshotProvider));
    let (tx, rx) = mpsc::channel::<(AiBridgeRequest, mpsc::Sender<AiBridgeResponse>)>();
    let listener = TcpListener::bind(("127.0.0.1", options.port));
    let (addr, token) = match listener {
        Ok(listener) => {
            let addr = listener
                .local_addr()
                .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], options.port)));
            if token.is_empty() {
                eprintln!("[AI Bridge] not listening: refusing to serve without a token");
            } else {
                let token_for_thread = token.clone();
                thread::spawn(move || serve(listener, tx, token_for_thread));
            }
            (addr, token)
        }
        Err(err) => {
            eprintln!(
                "[AI Bridge] Failed to bind 127.0.0.1:{}: {err}",
                options.port
            );
            (SocketAddr::from(([127, 0, 0, 1], options.port)), token)
        }
    };
    RunningBridge {
        handle: AiBridgeHandle {
            rx,
            screenshot_provider: provider,
            permissions,
            rate: RefCell::new(ActionRateLimiter::new()),
        },
        addr,
        token,
    }
}

fn permissions_from_env() -> AgentPermissions {
    match std::env::var("W3COS_AI_PERMISSIONS")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "observer" => AgentPermissions::observer(),
        "system" => AgentPermissions::system(),
        _ => AgentPermissions::interactive(),
    }
}

fn resolve_token(explicit: Option<String>) -> String {
    if let Some(token) = explicit.filter(|token| !token.is_empty()) {
        return token;
    }
    if let Ok(token) = std::env::var("W3COS_AI_TOKEN")
        && !token.is_empty()
    {
        return token;
    }
    fresh_token()
}

fn fresh_token() -> String {
    let mut bytes = [0u8; 32];
    match getrandom::fill(&mut bytes) {
        Ok(()) => hex_encode(&bytes),
        Err(err) => {
            eprintln!("[AI Bridge] token entropy unavailable ({err}); refusing listeners");
            String::new()
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn serve(
    listener: TcpListener,
    tx: mpsc::Sender<(AiBridgeRequest, mpsc::Sender<AiBridgeResponse>)>,
    token: String,
) {
    let addr = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    eprintln!("AI Bridge: http://{addr}  token={token}");
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        if let Err(err) = handle_connection(stream, &tx, &token) {
            eprintln!("[AI Bridge] Connection error: {err}");
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    tx: &mpsc::Sender<(AiBridgeRequest, mpsc::Sender<AiBridgeResponse>)>,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut headers = Vec::new();
    loop {
        if headers.len() >= MAX_HEADERS {
            send_response(
                &mut stream,
                400,
                "application/json",
                br#"{"error":"too many headers"}"#,
            )?;
            return Ok(());
        }
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        headers.push(line);
    }

    let content_length = headers
        .iter()
        .find_map(|header| {
            let lower = header.to_lowercase();
            if lower.starts_with("content-length:") {
                lower.split(':').nth(1)?.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        send_response(
            &mut stream,
            400,
            "application/json",
            br#"{"error":"request body too large"}"#,
        )?;
        return Ok(());
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    let body_str = String::from_utf8_lossy(&body).to_string();

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        send_response(&mut stream, 400, "text/plain", b"Bad Request")?;
        return Ok(());
    }
    let method = parts[0];
    let target = parts[1];
    let (path, query) = target
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((target, None));

    if !auth_exempt(method, path) && !authorized(&headers, query, token) {
        send_response(
            &mut stream,
            401,
            "application/json",
            br#"{"error":"unauthorized"}"#,
        )?;
        return Ok(());
    }

    let request = match (method, path) {
        ("GET", "/a11y") => Some(AiBridgeRequest::GetA11yTree),
        ("GET", "/a11y/json") => Some(AiBridgeRequest::GetA11yJson),
        ("GET", "/screenshot") => Some(AiBridgeRequest::Screenshot),
        ("POST", "/click") => match json_selector(&body_str) {
            Ok(selector) => Some(AiBridgeRequest::Click { selector }),
            Err(message) => {
                send_json_error(&mut stream, 400, message)?;
                return Ok(());
            }
        },
        ("POST", "/type") => {
            let body: serde_json::Value =
                serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null);
            let selector = body["selector"].as_str().unwrap_or("").to_string();
            let text = body["text"].as_str().unwrap_or("").to_string();
            if selector.is_empty() {
                send_json_error(&mut stream, 400, "missing 'selector' field")?;
                return Ok(());
            }
            Some(AiBridgeRequest::Type { selector, text })
        }
        ("POST", "/act") => match parse_act(&body_str) {
            Ok(request) => Some(request),
            Err(message) => {
                send_json_error(&mut stream, 400, message)?;
                return Ok(());
            }
        },
        ("GET", "/query") => {
            let selector = query
                .and_then(|qs| query_param(qs, "selector"))
                .unwrap_or_default();
            if selector.is_empty() {
                send_json_error(&mut stream, 400, "missing 'selector' query param")?;
                return Ok(());
            }
            Some(AiBridgeRequest::Query { selector })
        }
        ("GET", "/") | ("GET", "/health") => {
            let info = serde_json::json!({
                "service": "W3C OS AI Bridge",
                "version": "0.1.0",
                "auth": "bearer",
                "cors": "disabled",
                "endpoints": [
                    {"method": "GET", "path": "/a11y", "description": "Accessibility tree (text summary)"},
                    {"method": "GET", "path": "/a11y/json", "description": "Accessibility tree (full JSON)"},
                    {"method": "GET", "path": "/screenshot", "description": "PNG screenshot"},
                    {"method": "POST", "path": "/click", "description": "Click element by selector"},
                    {"method": "POST", "path": "/type", "description": "Type text into element"},
                    {"method": "POST", "path": "/act", "description": "Run a DOM action"},
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
                    send_response(
                        &mut stream,
                        200,
                        "text/plain; charset=utf-8",
                        text.as_bytes(),
                    )?;
                }
                AiBridgeResponse::Json(json) => {
                    send_response(&mut stream, 200, "application/json", json.as_bytes())?;
                }
                AiBridgeResponse::Png(data) => {
                    send_response(&mut stream, 200, "image/png", &data)?;
                }
                AiBridgeResponse::Error(msg) => {
                    send_json_error(&mut stream, 500, &msg)?;
                }
                AiBridgeResponse::Denied(msg) => {
                    send_json_error(&mut stream, 403, &msg)?;
                }
                AiBridgeResponse::RateLimited(msg) => {
                    send_json_error(&mut stream, 429, &msg)?;
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

fn auth_exempt(method: &str, path: &str) -> bool {
    method == "GET" && (path == "/" || path == "/health")
}

fn authorized(headers: &[String], query: Option<&str>, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    if let Some(value) = header_value(headers, "authorization")
        && let Some(presented) = bearer_token(value)
        && tokens_match(expected, presented)
    {
        return true;
    }
    query
        .and_then(|query| query_param(query, "token"))
        .is_some_and(|presented| tokens_match(expected, &presented))
}

fn header_value<'a>(headers: &'a [String], name: &str) -> Option<&'a str> {
    headers.iter().find_map(|header| {
        let (key, value) = header.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_end_matches(['\r', '\n']))
    })
}

fn bearer_token(value: &str) -> Option<&str> {
    let rest = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

fn tokens_match(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    let mut diff = expected.len() ^ presented.len();
    let len = expected.len().max(presented.len());
    for index in 0..len {
        let left = expected.get(index).copied().unwrap_or(0);
        let right = presented.get(index).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

fn json_selector(body: &str) -> Result<String, &'static str> {
    let body: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let selector = body["selector"].as_str().unwrap_or("").to_string();
    if selector.is_empty() {
        Err("missing 'selector' field")
    } else {
        Ok(selector)
    }
}

fn parse_act(body: &str) -> Result<AiBridgeRequest, &'static str> {
    let body: serde_json::Value = serde_json::from_str(body).map_err(|_| "invalid json")?;
    let name = body
        .get("action")
        .and_then(|value| value.as_str())
        .ok_or("missing 'action' field")?;
    let action: ActionType = serde_json::from_value(serde_json::Value::String(name.to_string()))
        .map_err(|_| "unknown action")?;
    let selector = body
        .get("selector")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if selector.is_empty() {
        return Err("missing 'selector' field");
    }
    let value = body
        .get("value")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    Ok(AiBridgeRequest::Act {
        action,
        selector,
        value,
    })
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|param| {
        let (name, value) = param.split_once('=')?;
        (name == key).then(|| urlish_decode(value))
    })
}

fn send_json_error(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
) -> Result<(), std::io::Error> {
    let body = serde_json::json!({"error": message}).to_string();
    send_response(stream, status, "application/json", body.as_bytes())
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
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        504 => "Gateway Timeout",
        _ => "Unknown",
    };
    // No Access-Control-Allow-Origin. This listener is a loopback agent API,
    // not a browser endpoint; a wildcard would let any web origin read it.
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn urlish_decode(value: &str) -> String {
    value
        .replace("%20", " ")
        .replace("%23", "#")
        .replace("%2E", ".")
        .replace("%2F", "/")
        .replace("%3A", ":")
        .replace("%3D", "=")
        .replace("%26", "&")
        .replace('+', " ")
}
