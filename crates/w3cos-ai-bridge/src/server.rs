use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::a11y_api;
use crate::dom_access::{self, ActionType, DomAction};
use crate::permissions::{self, ActionRateLimiter, AgentPermissions, AuthError, BridgeOp};
use crate::screenshot::{self, CaptureConfig, ElementAnnotation};
use w3cos_dom::document::Document;
use w3cos_dom::element::Element;
use w3cos_dom::node::NodeId;

/// Largest request body the bridge will buffer. Larger `Content-Length` values
/// are rejected before allocation.
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_HEADERS: usize = 64;

/// CSS selector or a 1-based row from `GET /a11y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomTarget {
    Selector(String),
    Index(u32),
}

/// Request from the HTTP server thread to the main event loop.
pub enum AiBridgeRequest {
    GetA11yTree,
    GetA11yJson,
    GetA11yIndex,
    Screenshot,
    AnnotatedScreenshot,
    Click {
        target: DomTarget,
    },
    Type {
        target: DomTarget,
        text: String,
    },
    Query {
        selector: String,
    },
    Act {
        action: ActionType,
        target: DomTarget,
        value: Option<String>,
    },
    Propose {
        utterance: String,
    },
    ConfirmIntent,
}

/// What a component-layout index did with an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutInvoke {
    Absent,
    Invoked,
    Rejected(&'static str),
}

/// Pixels plus layout boxes, or an explicit reason the annotated shot cannot
/// be drawn. An empty annotation list is not a successful capture.
pub enum AnnotatedFrame {
    Ready {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        annotations: Vec<ElementAnnotation>,
    },
    MissingLayout(&'static str),
}

/// Source of frame snapshots — supplied by the runtime so the bridge can
/// stay decoupled from the render pipeline.
pub trait ScreenshotProvider: Send + Sync {
    /// Return the latest framebuffer encoded as PNG. `None` when no frame
    /// has been rendered yet (or the provider can't capture in this mode).
    fn capture_png(&self) -> Option<Vec<u8>>;

    /// Annotated capture. The default refuses rather than inventing bounds.
    /// Called only while handling a bridge request, not while painting.
    fn capture_annotated(&self) -> AnnotatedFrame {
        AnnotatedFrame::MissingLayout(
            "layout bounds are not available for this frame; annotated screenshot was not drawn",
        )
    }

    /// Numbered component layout, when the DOM tree has no interactive nodes.
    fn layout_index_json(&self) -> Option<String> {
        None
    }

    /// Text form of [`layout_index_json`](Self::layout_index_json).
    fn layout_summary(&self) -> Option<String> {
        None
    }

    /// Activate a component hit by the layout index. Default: not a layout index.
    fn invoke_layout_index(&self, _index: u32, _action: ActionType) -> LayoutInvoke {
        LayoutInvoke::Absent
    }
}

static PREP_LAYOUT: AtomicBool = AtomicBool::new(false);
static PREP_PIXELS: AtomicBool = AtomicBool::new(false);

/// The HTTP thread asks the main thread to snapshot layout, and pixels when
/// the request is a screenshot. Painting itself does not watch this flag.
pub fn note_bridge_prep(layout: bool, pixels: bool) {
    if layout {
        PREP_LAYOUT.store(true, Ordering::Release);
    }
    if pixels {
        PREP_PIXELS.store(true, Ordering::Release);
    }
}

pub fn take_bridge_prep() -> (bool, bool) {
    (
        PREP_LAYOUT.swap(false, Ordering::AcqRel),
        PREP_PIXELS.swap(false, Ordering::AcqRel),
    )
}

type ProposeHook = fn(&str) -> String;
type ConfirmHook = fn() -> String;

fn propose_slot() -> &'static Mutex<Option<ProposeHook>> {
    static SLOT: OnceLock<Mutex<Option<ProposeHook>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn confirm_slot() -> &'static Mutex<Option<ConfirmHook>> {
    static SLOT: OnceLock<Mutex<Option<ConfirmHook>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Shell hook for `POST /intent`. The HTTP permission check still runs first.
pub fn set_intent_hooks(propose: fn(&str) -> String, confirm: fn() -> String) {
    if let Ok(mut slot) = propose_slot().lock() {
        *slot = Some(propose);
    }
    if let Ok(mut slot) = confirm_slot().lock() {
        *slot = Some(confirm);
    }
}

/// Response from the main event loop back to the HTTP server thread.
pub enum AiBridgeResponse {
    Text(String),
    Json(String),
    Png(Vec<u8>),
    Error(String),
    Denied(String),
    NotFound(String),
    Unavailable(String),
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
        if matches!(
            request,
            AiBridgeRequest::Propose { .. } | AiBridgeRequest::ConfirmIntent
        ) {
            if !self.permissions.can_interact {
                return AiBridgeResponse::Denied(AuthError::Interact.message("bridge"));
            }
            if !self
                .rate
                .borrow_mut()
                .try_acquire(now_ms(), self.permissions.max_actions_per_second)
            {
                return AiBridgeResponse::RateLimited(
                    "Agent 'bridge' exceeded max_actions_per_second".to_string(),
                );
            }
        } else if let Some(action) = mutation_action(&request) {
            if let Err(err) = permissions::authorize_action(&self.permissions, action) {
                return AiBridgeResponse::Denied(err.message("bridge"));
            }
            if let Some(selector) = declared_selector(&request)
                && !self.permissions.check_selector(&selector)
            {
                return AiBridgeResponse::Denied(AuthError::Selector.message("bridge"));
            }
            if !self
                .rate
                .borrow_mut()
                .try_acquire(now_ms(), self.permissions.max_actions_per_second)
            {
                return AiBridgeResponse::RateLimited(
                    "Agent 'bridge' exceeded max_actions_per_second".to_string(),
                );
            }
        } else {
            let op = match &request {
                AiBridgeRequest::GetA11yTree
                | AiBridgeRequest::GetA11yJson
                | AiBridgeRequest::GetA11yIndex => BridgeOp::ReadDom,
                AiBridgeRequest::Screenshot | AiBridgeRequest::AnnotatedScreenshot => {
                    BridgeOp::Screenshot
                }
                AiBridgeRequest::Query { selector } => BridgeOp::Query {
                    selector: selector.clone(),
                },
                AiBridgeRequest::Click { .. }
                | AiBridgeRequest::Type { .. }
                | AiBridgeRequest::Act { .. }
                | AiBridgeRequest::Propose { .. }
                | AiBridgeRequest::ConfirmIntent => {
                    unreachable!("mutations are handled above")
                }
            };
            if let Err(err) = permissions::authorize(&self.permissions, &op) {
                return AiBridgeResponse::Denied(err.message("bridge"));
            }
        }
        match request {
            AiBridgeRequest::GetA11yTree => {
                if !dom_has_interactive(doc)
                    && let Some(summary) = self.screenshot_provider.layout_summary()
                {
                    return AiBridgeResponse::Text(summary);
                }
                AiBridgeResponse::Text(a11y_api::get_ui_summary(doc))
            }
            AiBridgeRequest::GetA11yJson => AiBridgeResponse::Json(a11y_api::get_tree_json(doc)),
            AiBridgeRequest::GetA11yIndex => {
                if !dom_has_interactive(doc)
                    && let Some(json) = self.screenshot_provider.layout_index_json()
                {
                    return AiBridgeResponse::Json(json);
                }
                AiBridgeResponse::Json(a11y_api::index_json(doc))
            }
            AiBridgeRequest::Screenshot => match self.screenshot_provider.capture_png() {
                Some(png) => AiBridgeResponse::Png(png),
                None => AiBridgeResponse::Json(
                    r#"{"error":"no frame captured yet — render at least one frame first or enable CPU rendering"}"#
                        .to_string(),
                ),
            },
            AiBridgeRequest::AnnotatedScreenshot => self.annotated_screenshot(),
            AiBridgeRequest::Click { target } => mutate(
                doc,
                &self.permissions,
                self.screenshot_provider.as_ref(),
                ActionType::Click,
                &target,
                None,
            ),
            AiBridgeRequest::Type { target, text } => mutate(
                doc,
                &self.permissions,
                self.screenshot_provider.as_ref(),
                ActionType::SetText,
                &target,
                Some(text),
            ),
            AiBridgeRequest::Query { selector } => {
                let result = dom_access::query(doc, &selector);
                AiBridgeResponse::Json(serde_json::to_string(&result).unwrap_or_default())
            }
            AiBridgeRequest::Act {
                action,
                target,
                value,
            } => mutate(
                doc,
                &self.permissions,
                self.screenshot_provider.as_ref(),
                action,
                &target,
                value,
            ),
            AiBridgeRequest::Propose { utterance } => propose_intent(&utterance),
            AiBridgeRequest::ConfirmIntent => confirm_intent(),
        }
    }

    fn annotated_screenshot(&self) -> AiBridgeResponse {
        match self.screenshot_provider.capture_annotated() {
            AnnotatedFrame::MissingLayout(reason) => AiBridgeResponse::Unavailable(reason.into()),
            AnnotatedFrame::Ready {
                width,
                height,
                rgba,
                annotations,
            } => {
                if width == 0
                    || height == 0
                    || annotations.is_empty()
                    || annotations
                        .iter()
                        .any(|entry| entry.bounds[2] <= 0.0 || entry.bounds[3] <= 0.0)
                {
                    return AiBridgeResponse::Unavailable(
                        "layout bounds are missing; refusing an empty annotation map".into(),
                    );
                }
                let shot = screenshot::capture(
                    &rgba,
                    width,
                    height,
                    annotations,
                    &CaptureConfig {
                        annotate_interactive: true,
                        show_outlines: false,
                        include_map: true,
                    },
                );
                AiBridgeResponse::Json(screenshot::annotated_json(&shot))
            }
        }
    }
}

fn mutation_action(request: &AiBridgeRequest) -> Option<ActionType> {
    match request {
        AiBridgeRequest::Click { .. } => Some(ActionType::Click),
        AiBridgeRequest::Type { .. } => Some(ActionType::SetText),
        AiBridgeRequest::Act { action, .. } => Some(*action),
        _ => None,
    }
}

fn declared_selector(request: &AiBridgeRequest) -> Option<String> {
    let target = match request {
        AiBridgeRequest::Click { target }
        | AiBridgeRequest::Type { target, .. }
        | AiBridgeRequest::Act { target, .. } => target,
        _ => return None,
    };
    match target {
        DomTarget::Selector(selector) => Some(selector.clone()),
        DomTarget::Index(_) => None,
    }
}

fn dom_has_interactive(doc: &Document) -> bool {
    a11y_api::index_entries(doc)
        .iter()
        .any(|entry| entry.interactive)
}

fn propose_intent(utterance: &str) -> AiBridgeResponse {
    let hook = propose_slot().lock().ok().and_then(|slot| *slot);
    let Some(hook) = hook else {
        return AiBridgeResponse::Error("intent bridge is not registered".into());
    };
    let summary = hook(utterance);
    AiBridgeResponse::Json(serde_json::json!({"ok": true, "summary": summary}).to_string())
}

fn confirm_intent() -> AiBridgeResponse {
    let hook = confirm_slot().lock().ok().and_then(|slot| *slot);
    let Some(hook) = hook else {
        return AiBridgeResponse::Error("intent bridge is not registered".into());
    };
    let summary = hook();
    AiBridgeResponse::Json(serde_json::json!({"ok": true, "summary": summary}).to_string())
}

fn mutate(
    doc: &mut Document,
    permissions: &AgentPermissions,
    provider: &dyn ScreenshotProvider,
    action: ActionType,
    target: &DomTarget,
    value: Option<String>,
) -> AiBridgeResponse {
    let result = match target {
        DomTarget::Selector(selector) => {
            let action = DomAction {
                action,
                selector: selector.clone(),
                value,
            };
            dom_access::execute(doc, &action)
        }
        DomTarget::Index(index) => {
            if !dom_has_interactive(doc) {
                return match provider.invoke_layout_index(*index, action) {
                    LayoutInvoke::Invoked => {
                        AiBridgeResponse::Json(r#"{"success":true}"#.to_string())
                    }
                    LayoutInvoke::Rejected(message) => AiBridgeResponse::Json(
                        serde_json::json!({"success": false, "error": message}).to_string(),
                    ),
                    LayoutInvoke::Absent => AiBridgeResponse::NotFound(format!(
                        "a11y index {index} is not in the current tree"
                    )),
                };
            }
            let Some(entry) = a11y_api::index_entries(doc)
                .into_iter()
                .find(|entry| entry.index == *index)
            else {
                return AiBridgeResponse::NotFound(format!(
                    "a11y index {index} is not in the current tree"
                ));
            };
            let selector = index_permission_selector(doc, &entry);
            if !permissions.check_selector(&selector) {
                return AiBridgeResponse::Denied(AuthError::Selector.message("bridge"));
            }
            dom_access::execute_node(doc, entry.node_id, action, value.as_deref())
        }
    };
    AiBridgeResponse::Json(serde_json::to_string(&result).unwrap_or_default())
}

fn index_permission_selector(doc: &Document, entry: &w3cos_a11y::tree::FlatEntry) -> String {
    let id = Element::new(NodeId::from_u32(entry.node_id))
        .get_attribute(doc, "id")
        .unwrap_or("");
    let name = entry.name.as_deref().unwrap_or("");
    format!("#{id} {} {name}", entry.role)
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
    // Unset and empty stay observer. interactive|system must be explicit.
    let raw = std::env::var("W3COS_AI_PERMISSIONS").unwrap_or_default();
    if !crate::permissions::permissions_label_is_known(&raw) {
        eprintln!("[AI Bridge] unknown W3COS_AI_PERMISSIONS={raw:?}; using observer (fail closed)");
    }
    let permissions = crate::permissions::permissions_from_label(&raw);
    let mode = if permissions.can_spawn_processes {
        "system"
    } else if permissions.can_interact {
        "interactive"
    } else {
        "observer"
    };
    eprintln!("[AI Bridge] permissions={mode}");
    permissions
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
        ("GET", "/a11y/index") => Some(AiBridgeRequest::GetA11yIndex),
        ("GET", "/screenshot") => Some(AiBridgeRequest::Screenshot),
        ("GET", "/screenshot/annotated") => Some(AiBridgeRequest::AnnotatedScreenshot),
        ("POST", "/click") => match json_target(&body_str) {
            Ok(target) => Some(AiBridgeRequest::Click { target }),
            Err(message) => {
                send_json_error(&mut stream, 400, message)?;
                return Ok(());
            }
        },
        ("POST", "/type") => match json_target_and_text(&body_str) {
            Ok((target, text)) => Some(AiBridgeRequest::Type { target, text }),
            Err(message) => {
                send_json_error(&mut stream, 400, message)?;
                return Ok(());
            }
        },
        ("POST", "/intent") => match json_utterance(&body_str) {
            Ok(utterance) => Some(AiBridgeRequest::Propose { utterance }),
            Err(message) => {
                send_json_error(&mut stream, 400, message)?;
                return Ok(());
            }
        },
        ("POST", "/intent/confirm") => Some(AiBridgeRequest::ConfirmIntent),
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
                    {"method": "GET", "path": "/a11y/index", "description": "Numbered a11y index (no screenshot)"},
                    {"method": "GET", "path": "/screenshot", "description": "PNG screenshot"},
                    {"method": "GET", "path": "/screenshot/annotated", "description": "Annotated PNG plus index map, or 503 when bounds are missing"},
                    {"method": "POST", "path": "/click", "description": "Click element by selector or a11y index"},
                    {"method": "POST", "path": "/type", "description": "Type text into element by selector or a11y index"},
                    {"method": "POST", "path": "/act", "description": "Run a DOM action"},
                    {"method": "POST", "path": "/intent", "description": "Propose an L0 intent utterance"},
                    {"method": "POST", "path": "/intent/confirm", "description": "Confirm the pending intent"},
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
        match &req {
            AiBridgeRequest::GetA11yTree
            | AiBridgeRequest::GetA11yJson
            | AiBridgeRequest::GetA11yIndex => note_bridge_prep(true, false),
            AiBridgeRequest::Screenshot | AiBridgeRequest::AnnotatedScreenshot => {
                note_bridge_prep(true, true)
            }
            _ => {}
        }
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
                AiBridgeResponse::NotFound(msg) => {
                    send_json_error(&mut stream, 404, &msg)?;
                }
                AiBridgeResponse::Unavailable(msg) => {
                    send_json_error(&mut stream, 503, &msg)?;
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

fn json_utterance(body: &str) -> Result<String, &'static str> {
    let body: serde_json::Value = serde_json::from_str(body).map_err(|_| "invalid json")?;
    let utterance = body
        .get("utterance")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if utterance.is_empty() {
        Err("missing 'utterance'")
    } else {
        Ok(utterance)
    }
}

fn json_target(body: &str) -> Result<DomTarget, &'static str> {
    let body: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    target_from_json(&body)
}

fn json_target_and_text(body: &str) -> Result<(DomTarget, String), &'static str> {
    let body: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let target = target_from_json(&body)?;
    let text = body
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    Ok((target, text))
}

fn target_from_json(body: &serde_json::Value) -> Result<DomTarget, &'static str> {
    let selector = body
        .get("selector")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let index = body.get("index");
    match (selector.is_empty(), index) {
        (false, Some(_)) => Err("provide 'selector' or 'index', not both"),
        (true, None) => Err("missing 'selector' or 'index'"),
        (false, None) => Ok(DomTarget::Selector(selector)),
        (true, Some(value)) => {
            let index = value.as_u64().ok_or("index must be a positive integer")?;
            let index = u32::try_from(index).map_err(|_| "index must be a positive integer")?;
            if index == 0 {
                Err("index must be a positive integer")
            } else {
                Ok(DomTarget::Index(index))
            }
        }
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
    let target = target_from_json(&body)?;
    let value = body
        .get("value")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    Ok(AiBridgeRequest::Act {
        action,
        target,
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
        503 => "Service Unavailable",
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
