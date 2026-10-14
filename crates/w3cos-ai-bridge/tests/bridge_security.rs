use std::cell::Cell;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use w3cos_ai_bridge::dom_access::ActionType;
use w3cos_ai_bridge::permissions::AgentPermissions;
use w3cos_ai_bridge::screenshot::ElementAnnotation;
use w3cos_ai_bridge::server::{
    AnnotatedFrame, BridgeOptions, LayoutInvoke, RunningBridge, ScreenshotProvider,
    start_with_options,
};
use w3cos_dom::Document;

struct FlagShot {
    called: Arc<AtomicBool>,
}

impl ScreenshotProvider for FlagShot {
    fn capture_png(&self) -> Option<Vec<u8>> {
        self.called.store(true, Ordering::SeqCst);
        Some(b"png".to_vec())
    }
}

fn button() -> (Document, Rc<Cell<u32>>) {
    let mut doc = Document::new();
    let button = doc.create_element("button");
    button.set_attribute(&mut doc, "id", "go");
    button.set_text_content(&mut doc, "Go");
    let hits = Rc::new(Cell::new(0u32));
    let hits_for_handler = hits.clone();
    button.add_event_listener(
        &mut doc,
        "click",
        Box::new(move |_| {
            hits_for_handler.set(hits_for_handler.get() + 1);
        }),
    );
    doc.body().append_child(&mut doc, button);
    (doc, hits)
}

fn serve(permissions: AgentPermissions) -> RunningBridge {
    start_with_options(BridgeOptions {
        port: 0,
        permissions,
        token: Some("test-token-secret".into()),
        screenshot: None,
    })
}

fn serve_with_shot(
    permissions: AgentPermissions,
    shot: Arc<dyn ScreenshotProvider>,
) -> RunningBridge {
    start_with_options(BridgeOptions {
        port: 0,
        permissions,
        token: Some("test-token-secret".into()),
        screenshot: Some(shot),
    })
}

fn exchange(
    bridge: &RunningBridge,
    doc: &mut Document,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String, String) {
    let addr = bridge.addr;
    let method = method.to_string();
    let path = path.to_string();
    let token = token.map(str::to_string);
    let body = body.map(str::to_string);
    let (tx, rx) = mpsc::channel();
    let method_for_thread = method.clone();
    let path_for_thread = path.clone();
    thread::spawn(move || {
        let result = http(
            addr,
            &method_for_thread,
            &path_for_thread,
            token.as_deref(),
            body.as_deref(),
        );
        let _ = tx.send(result);
    });
    let started = Instant::now();
    loop {
        bridge.handle.poll_and_respond(doc);
        if let Ok(result) = rx.try_recv() {
            return result;
        }
        if started.elapsed() > Duration::from_secs(3) {
            panic!("timed out waiting for {method} {path}");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let body = body.unwrap_or("");
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap_or(0);
    (status, head.to_string(), body.to_string())
}

fn assert_no_cors(headers: &str) {
    assert!(
        !headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin"),
        "{headers}"
    );
}

#[test]
fn health_is_public_and_does_not_leak_the_token_or_cors() {
    let bridge = serve(AgentPermissions::interactive());
    assert!(bridge.addr.ip().is_loopback());
    let (mut doc, _) = button();
    let (status, headers, body) = exchange(&bridge, &mut doc, "GET", "/health", None, None);
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains("test-token-secret"), "{body}");
    assert!(body.contains("disabled"), "{body}");
    assert!(body.contains("/act"), "{body}");
    assert_no_cors(&headers);
}

#[test]
fn missing_or_wrong_token_is_401_and_does_not_click() {
    let bridge = serve(AgentPermissions::interactive());
    let (mut doc, hits) = button();
    let (status, headers, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        None,
        Some("{\"selector\":\"#go\"}"),
    );
    assert_eq!(status, 401, "{body}");
    assert_no_cors(&headers);
    assert_eq!(hits.get(), 0);

    let (status, _, body) = exchange(&bridge, &mut doc, "GET", "/a11y", Some("nope"), None);
    assert_eq!(status, 401, "{body}");
    assert_eq!(hits.get(), 0);
}

#[test]
fn bearer_token_allows_a_read() {
    let bridge = serve(AgentPermissions::observer());
    let (mut doc, _) = button();
    let (status, headers, body) = exchange(
        &bridge,
        &mut doc,
        "GET",
        "/a11y",
        Some("test-token-secret"),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert!(!body.is_empty(), "{body}");
    assert_no_cors(&headers);
}

#[test]
fn query_token_is_accepted() {
    let bridge = serve(AgentPermissions::observer());
    let (mut doc, _) = button();
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "GET",
        "/query?selector=%23go&token=test-token-secret",
        None,
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"success\":true"), "{body}");
}

#[test]
fn observer_cannot_click_and_the_document_stays_put() {
    let bridge = serve(AgentPermissions::observer());
    let (mut doc, hits) = button();
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some("test-token-secret"),
        Some("{\"selector\":\"#go\"}"),
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("interaction"), "{body}");
    assert_eq!(hits.get(), 0);
}

#[test]
fn blocked_selector_is_rejected() {
    let mut perms = AgentPermissions::interactive();
    perms.blocked_selectors.push("#go".into());
    let bridge = serve(perms);
    let (mut doc, hits) = button();
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some("test-token-secret"),
        Some("{\"selector\":\"#go\"}"),
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("selector"), "{body}");
    assert_eq!(hits.get(), 0);
}

#[test]
fn interactive_click_fires_and_then_rate_limits() {
    let mut perms = AgentPermissions::interactive();
    perms.max_actions_per_second = 1;
    let bridge = serve(perms);
    let (mut doc, hits) = button();
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some("test-token-secret"),
        Some("{\"selector\":\"#go\"}"),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(hits.get(), 1);
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some("test-token-secret"),
        Some("{\"selector\":\"#go\"}"),
    );
    assert_eq!(status, 429, "{body}");
    assert_eq!(hits.get(), 1);
}

#[test]
fn type_requires_write_permission() {
    let bridge = serve(AgentPermissions::default());
    let (mut doc, _) = button();
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/type",
        Some("test-token-secret"),
        Some("{\"selector\":\"#go\",\"text\":\"nope\"}"),
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("write"), "{body}");
    let current = doc
        .query_selector("#go")
        .unwrap()
        .get_attribute(&doc, "id")
        .unwrap_or("");
    assert_eq!(current, "go");
    assert_eq!(doc.query_selector("#go").unwrap().inner_text(&doc), "Go");
}

#[test]
fn act_sets_an_attribute_and_rejects_unknown_actions() {
    let bridge = serve(AgentPermissions::interactive());
    let (mut doc, hits) = button();
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/act",
        Some("test-token-secret"),
        Some("{\"action\":\"dance\",\"selector\":\"#go\"}"),
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(hits.get(), 0);

    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/act",
        Some("test-token-secret"),
        Some("{\"action\":\"focus\",\"selector\":\"#go\"}"),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"success\":true"), "{body}");
    assert_eq!(hits.get(), 0);

    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "POST",
        "/act",
        Some("test-token-secret"),
        Some("{\"action\":\"set_attribute\",\"selector\":\"#go\",\"value\":\"data-role=primary\"}"),
    );
    assert_eq!(status, 200, "{body}");
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "GET",
        "/query?selector=%23go",
        Some("test-token-secret"),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("data-role"), "{body}");
    assert!(body.contains("primary"), "{body}");
}

#[test]
fn screenshot_without_permission_does_not_capture() {
    let called = Arc::new(AtomicBool::new(false));
    let bridge = serve_with_shot(
        {
            let mut perms = AgentPermissions::observer();
            perms.can_screenshot = false;
            perms
        },
        Arc::new(FlagShot {
            called: called.clone(),
        }),
    );
    let (mut doc, _) = button();
    let (status, _, body) = exchange(
        &bridge,
        &mut doc,
        "GET",
        "/screenshot",
        Some("test-token-secret"),
        None,
    );
    assert_eq!(status, 403, "{body}");
    assert!(!called.load(Ordering::SeqCst));
}

fn index_of(summary: &str, needle: &str) -> u32 {
    summary
        .lines()
        .find(|line| line.contains(needle))
        .and_then(|line| {
            let rest = line.trim().strip_prefix('[')?;
            rest.split(']').next()?.parse().ok()
        })
        .unwrap_or_else(|| panic!("no index for {needle} in {summary}"))
}

fn token(
    bridge: &RunningBridge,
    doc: &mut Document,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let (status, _, body) = exchange(bridge, doc, method, path, Some("test-token-secret"), body);
    (status, body)
}

struct AnnotatedShot {
    called: Arc<AtomicBool>,
}

impl ScreenshotProvider for AnnotatedShot {
    fn capture_png(&self) -> Option<Vec<u8>> {
        None
    }

    fn capture_annotated(&self) -> AnnotatedFrame {
        self.called.store(true, Ordering::SeqCst);
        AnnotatedFrame::Ready {
            width: 32,
            height: 32,
            rgba: vec![255u8; 32 * 32 * 4],
            annotations: vec![
                ElementAnnotation {
                    index: 1,
                    node_id: 4,
                    label: "button: Go".into(),
                    bounds: [1.0, 1.0, 8.0, 8.0],
                    interactive: true,
                },
                ElementAnnotation {
                    index: 2,
                    node_id: 5,
                    label: "button: Stop".into(),
                    bounds: [16.0, 8.0, 8.0, 8.0],
                    interactive: true,
                },
            ],
        }
    }
}

#[test]
fn observer_can_read_the_a11y_index_but_cannot_click_it() {
    let bridge = serve(AgentPermissions::observer());
    let (mut doc, hits) = button();
    let (status, body) = token(&bridge, &mut doc, "GET", "/a11y", None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("button: Go"), "{body}");
    let index = index_of(&body, "button: Go");
    let (status, index_body) = token(&bridge, &mut doc, "GET", "/a11y/index", None);
    assert_eq!(status, 200, "{index_body}");
    assert!(index_body.contains("\"index\":"), "{index_body}");
    assert!(
        index_body.contains(&format!("\"index\":{index}")),
        "{index_body}"
    );
    assert!(index_body.contains("button"), "{index_body}");
    assert!(index_body.contains("Go"), "{index_body}");
    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some(&format!("{{\"index\":{index}}}")),
    );
    assert_eq!(status, 403, "{body}");
    assert_eq!(hits.get(), 0);
}

#[test]
fn interactive_click_type_and_focus_accept_an_a11y_index() {
    let bridge = serve(AgentPermissions::interactive());
    let (mut doc, hits) = button();
    let input = doc.create_element("input");
    input.set_attribute(&mut doc, "id", "name");
    input.set_text_content(&mut doc, "Name");
    doc.body().append_child(&mut doc, input);

    let (status, summary) = token(&bridge, &mut doc, "GET", "/a11y", None);
    assert_eq!(status, 200, "{summary}");
    let (again_status, again) = token(&bridge, &mut doc, "GET", "/a11y", None);
    assert_eq!(again_status, 200);
    assert_eq!(summary, again, "numbering changed without a tree edit");
    let button_index = index_of(&summary, "button: Go");
    let input_index = index_of(&summary, "textbox: Name");

    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some(&format!("{{\"index\":{button_index}}}")),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"success\":true"), "{body}");
    assert_eq!(hits.get(), 1);

    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/type",
        Some(&format!("{{\"index\":{input_index},\"text\":\"typed\"}}")),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"success\":true"), "{body}");
    let (status, body) = token(&bridge, &mut doc, "GET", "/query?selector=%23name", None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("typed"), "{body}");
    assert_eq!(hits.get(), 1);

    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/act",
        Some(&format!(
            "{{\"action\":\"focus\",\"index\":{button_index}}}"
        )),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"success\":true"), "{body}");
    assert_eq!(hits.get(), 1);
}

#[test]
fn stale_or_ambiguous_index_does_not_mutate() {
    let bridge = serve(AgentPermissions::interactive());
    let (mut doc, hits) = button();
    let (status, body) = token(&bridge, &mut doc, "POST", "/click", Some("{\"index\":999}"));
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("not in the current tree"), "{body}");
    assert_eq!(hits.get(), 0);

    let (status, body) = token(&bridge, &mut doc, "POST", "/click", Some("{\"index\":0}"));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("positive integer"), "{body}");
    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some("{\"selector\":\"#go\",\"index\":1}"),
    );
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("not both"), "{body}");
    let (status, body) = token(&bridge, &mut doc, "POST", "/click", Some("{}"));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("missing"), "{body}");
    assert_eq!(hits.get(), 0);
}

#[test]
fn blocked_selector_denies_the_resolved_index() {
    let mut perms = AgentPermissions::interactive();
    perms.blocked_selectors.push("#go".into());
    let bridge = serve(perms);
    let (mut doc, hits) = button();
    let (status, summary) = token(&bridge, &mut doc, "GET", "/a11y", None);
    assert_eq!(status, 200, "{summary}");
    let index = index_of(&summary, "button: Go");
    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/click",
        Some(&format!("{{\"index\":{index}}}")),
    );
    assert_eq!(status, 403, "{body}");
    assert_eq!(hits.get(), 0);
}

#[test]
fn annotated_screenshot_needs_layout_bounds_and_capture_permission() {
    let bridge = serve(AgentPermissions::observer());
    let (mut doc, _) = button();
    let (status, raw) = token(&bridge, &mut doc, "GET", "/screenshot", None);
    assert_eq!(status, 200, "{raw}");
    assert!(raw.contains("no frame"), "{raw}");
    let (status, body) = token(&bridge, &mut doc, "GET", "/screenshot/annotated", None);
    assert_eq!(status, 503, "{body}");
    assert!(body.contains("layout bounds"), "{body}");

    let called = Arc::new(AtomicBool::new(false));
    let bridge = serve_with_shot(
        AgentPermissions::observer(),
        Arc::new(AnnotatedShot {
            called: called.clone(),
        }),
    );
    let (status, body) = token(&bridge, &mut doc, "GET", "/screenshot/annotated", None);
    assert_eq!(status, 200, "{body}");
    assert!(called.load(Ordering::SeqCst));
    assert!(body.contains("\"index\":1"), "{body}");
    assert!(body.contains("\"index\":2"), "{body}");
    assert!(body.contains("png_base64"), "{body}");
    assert!(!body.contains("\"annotations\":[]"), "{body}");

    let called = Arc::new(AtomicBool::new(false));
    let mut perms = AgentPermissions::observer();
    perms.can_screenshot = false;
    let bridge = serve_with_shot(
        perms,
        Arc::new(AnnotatedShot {
            called: called.clone(),
        }),
    );
    let (status, body) = token(&bridge, &mut doc, "GET", "/screenshot/annotated", None);
    assert_eq!(status, 403, "{body}");
    assert!(!called.load(Ordering::SeqCst));
}

struct LayoutIndex {
    invoked: Arc<AtomicBool>,
}

impl ScreenshotProvider for LayoutIndex {
    fn capture_png(&self) -> Option<Vec<u8>> {
        None
    }

    fn capture_annotated(&self) -> AnnotatedFrame {
        AnnotatedFrame::Ready {
            width: 16,
            height: 16,
            rgba: vec![255u8; 16 * 16 * 4],
            annotations: vec![ElementAnnotation {
                index: 1,
                node_id: 9,
                label: "button: Terminal".into(),
                bounds: [2.0, 2.0, 8.0, 8.0],
                interactive: true,
            }],
        }
    }

    fn layout_index_json(&self) -> Option<String> {
        Some(
            r#"[{"index":1,"node_id":9,"role":"button","name":"Terminal","interactive":true}]"#
                .into(),
        )
    }

    fn layout_summary(&self) -> Option<String> {
        Some("UI: 1 elements, 1 interactive\n---\n[1] button: Terminal [interactive]\n".into())
    }

    fn invoke_layout_index(&self, index: u32, action: ActionType) -> LayoutInvoke {
        if index == 1 && matches!(action, ActionType::Click | ActionType::Focus) {
            self.invoked.store(true, Ordering::SeqCst);
            LayoutInvoke::Invoked
        } else if index == 1 {
            LayoutInvoke::Rejected("component layout index accepts click or focus")
        } else {
            LayoutInvoke::Absent
        }
    }
}

fn intent_propose(utterance: &str) -> String {
    format!("proposed {utterance}")
}

fn intent_confirm() -> String {
    "confirmed".into()
}

#[test]
fn component_layout_index_is_readable_and_clickable() {
    let invoked = Arc::new(AtomicBool::new(false));
    let bridge = serve_with_shot(
        AgentPermissions::observer(),
        Arc::new(LayoutIndex {
            invoked: invoked.clone(),
        }),
    );
    let mut doc = Document::new();
    let (status, summary) = token(&bridge, &mut doc, "GET", "/a11y", None);
    assert_eq!(status, 200, "{summary}");
    assert!(summary.contains("button: Terminal"), "{summary}");
    let (status, body) = token(&bridge, &mut doc, "GET", "/a11y/index", None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"index\":1"), "{body}");
    let (status, body) = token(&bridge, &mut doc, "POST", "/click", Some("{\"index\":1}"));
    assert_eq!(status, 403, "{body}");
    assert!(!invoked.load(Ordering::SeqCst));

    let bridge = serve_with_shot(
        AgentPermissions::interactive(),
        Arc::new(LayoutIndex {
            invoked: invoked.clone(),
        }),
    );
    let (status, body) = token(&bridge, &mut doc, "POST", "/click", Some("{\"index\":1}"));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"success\":true"), "{body}");
    assert!(invoked.load(Ordering::SeqCst));
    let (status, body) = token(&bridge, &mut doc, "GET", "/screenshot/annotated", None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("png_base64"), "{body}");
    assert!(body.contains("\"index\":1"), "{body}");
}

#[test]
fn observer_cannot_propose_an_intent() {
    w3cos_ai_bridge::server::set_intent_hooks(intent_propose, intent_confirm);
    let bridge = serve(AgentPermissions::observer());
    let mut doc = Document::new();
    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/intent",
        Some("{\"utterance\":\"打开终端\"}"),
    );
    assert_eq!(status, 403, "{body}");
    let bridge = serve(AgentPermissions::interactive());
    let (status, body) = token(
        &bridge,
        &mut doc,
        "POST",
        "/intent",
        Some("{\"utterance\":\"打开终端\"}"),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("proposed 打开终端"), "{body}");
    let (status, body) = token(&bridge, &mut doc, "POST", "/intent/confirm", None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("confirmed"), "{body}");
    let (status, body) = token(&bridge, &mut doc, "POST", "/intent", Some("{}"));
    assert_eq!(status, 400, "{body}");
}
