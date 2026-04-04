use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use tungstenite::protocol::Message;
use tungstenite::{accept, WebSocket};

use super::cdp::{CdpEvent, CdpHandler, CdpRequest, CdpResponse};
use serde_json::json;
use crate::layout::LayoutRect;

// ---------------------------------------------------------------------------
// Snapshot — thread-safe serialized DOM state from the main thread
// ---------------------------------------------------------------------------

pub struct DomSnapshot {
    pub serialized_doc: SerializedDocument,
    pub layout_rects: Vec<(LayoutRect, usize)>,
}

/// Thread-safe representation of the DOM tree (no closures/event handlers).
pub struct SerializedDocument {
    pub nodes: Vec<SerializedNode>,
}

pub struct SerializedNode {
    pub id: u32,
    pub node_type: u8, // 1=Element, 3=Text, 9=Document
    pub tag: String,
    pub text_content: Option<String>,
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    pub attributes: Vec<(String, String)>,
    pub class_list: Vec<String>,
    pub style: w3cos_std::style::Style,
}

impl SerializedDocument {
    pub fn from_document(doc: &w3cos_dom::Document) -> Self {
        let mut nodes = Vec::new();
        Self::serialize_recursive(doc, w3cos_dom::NodeId::ROOT, &mut nodes);
        SerializedDocument { nodes }
    }

    fn serialize_recursive(
        doc: &w3cos_dom::Document,
        id: w3cos_dom::NodeId,
        nodes: &mut Vec<SerializedNode>,
    ) {
        let node = doc.get_node(id);
        let style = doc.get_style(id).to_style();
        let children_ids = doc.children_ids(id);

        let node_type = match node.node_type {
            w3cos_dom::node::NodeType::Document => 9,
            w3cos_dom::node::NodeType::Element => 1,
            w3cos_dom::node::NodeType::Text => 3,
        };

        let attrs: Vec<(String, String)> = node
            .attributes
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();

        let class_list: Vec<String> = node.class_list.iter().map(|a| a.as_str()).collect();

        nodes.push(SerializedNode {
            id: id.as_u32(),
            node_type,
            tag: node.tag.as_str(),
            text_content: node.text_content.clone(),
            parent: node.parent.map(|p| p.as_u32()),
            children: children_ids.iter().map(|c| c.as_u32()).collect(),
            attributes: attrs,
            class_list,
            style,
        });

        for cid in children_ids {
            Self::serialize_recursive(doc, cid, nodes);
        }
    }

    pub fn get_node(&self, id: u32) -> Option<&SerializedNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

// ---------------------------------------------------------------------------
// Messages between main thread and devtools thread
// ---------------------------------------------------------------------------

pub enum MainToDevTools {
    Snapshot(DomSnapshot),
    Shutdown,
}

pub enum DevToolsToMain {
    HighlightNode(Option<i64>),
    RequestSnapshot,
}

// ---------------------------------------------------------------------------
// DevToolsHandle — held by the main (winit) thread
// ---------------------------------------------------------------------------

pub struct DevToolsHandle {
    pub to_devtools: mpsc::Sender<MainToDevTools>,
    pub from_devtools: mpsc::Receiver<DevToolsToMain>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl DevToolsHandle {
    pub fn poll_messages(&self) -> Vec<DevToolsToMain> {
        let mut msgs = Vec::new();
        while let Ok(msg) = self.from_devtools.try_recv() {
            msgs.push(msg);
        }
        msgs
    }

    pub fn send_snapshot(&self, snapshot: DomSnapshot) {
        let _ = self.to_devtools.send(MainToDevTools::Snapshot(snapshot));
    }

    pub fn shutdown(mut self) {
        let _ = self.to_devtools.send(MainToDevTools::Shutdown);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for DevToolsHandle {
    fn drop(&mut self) {
        let _ = self.to_devtools.send(MainToDevTools::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// DevToolsServer — runs on a background thread
// ---------------------------------------------------------------------------

pub struct DevToolsServer;

impl DevToolsServer {
    pub fn start(port: u16) -> DevToolsHandle {
        let (main_tx, devtools_rx) = mpsc::channel::<MainToDevTools>();
        let (devtools_tx, main_rx) = mpsc::channel::<DevToolsToMain>();

        let join_handle = thread::spawn(move || {
            Self::run(port, devtools_rx, devtools_tx);
        });

        eprintln!("[DevTools] Chrome DevTools listening on ws://127.0.0.1:{port}");
        eprintln!(
            "[DevTools] Open chrome://inspect or edge://inspect and configure target 127.0.0.1:{port}"
        );
        eprintln!(
            "[DevTools] Or open: devtools://devtools/bundled/inspector.html?ws=127.0.0.1:{port}"
        );

        DevToolsHandle {
            to_devtools: main_tx,
            from_devtools: main_rx,
            join_handle: Some(join_handle),
        }
    }

    fn run(
        port: u16,
        rx: mpsc::Receiver<MainToDevTools>,
        tx: mpsc::Sender<DevToolsToMain>,
    ) {
        let listener = match TcpListener::bind(format!("127.0.0.1:{port}")) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[DevTools] Failed to bind port {port}: {e}");
                return;
            }
        };

        listener
            .set_nonblocking(false)
            .expect("set_nonblocking failed");

        let latest_snapshot: Arc<Mutex<Option<DomSnapshot>>> = Arc::new(Mutex::new(None));

        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let _ = stream.set_nonblocking(false);

            let mut peek_buf = [0u8; 512];
            let is_http = if let Ok(n) = stream.peek(&mut peek_buf) {
                let preview = String::from_utf8_lossy(&peek_buf[..n]);
                preview.contains("GET /json")
                    && !preview.contains("Upgrade: websocket")
                    && !preview.contains("upgrade: websocket")
            } else {
                false
            };

            if is_http {
                Self::handle_http(stream, port);
            } else {
                Self::handle_websocket(stream, &rx, &tx, &latest_snapshot);
            }
        }
    }

    fn handle_http(mut stream: TcpStream, port: u16) {
        let mut buf = [0u8; 2048];
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(_) => return,
        };
        let request = String::from_utf8_lossy(&buf[..n]);

        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/");

        let response_body = match path {
            "/json/version" => json!({
                "Browser": "W3COS/0.1.0",
                "Protocol-Version": "1.3",
                "V8-Version": "0.0.0",
                "User-Agent": "W3COS",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}")
            })
            .to_string(),

            "/json" | "/json/list" => json!([{
                "description": "W3C OS Application",
                "devtoolsFrontendUrl": format!(
                    "devtools://devtools/bundled/inspector.html?ws=127.0.0.1:{port}"
                ),
                "id": "w3cos-main",
                "title": "W3C OS App",
                "type": "page",
                "url": "w3cos://app",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}")
            }])
            .to_string(),

            _ => "{}".to_string(),
        };

        let http_response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            response_body.len(),
            response_body
        );
        let _ = stream.write_all(http_response.as_bytes());
    }

    fn handle_websocket(
        stream: TcpStream,
        rx: &mpsc::Receiver<MainToDevTools>,
        tx: &mpsc::Sender<DevToolsToMain>,
        snapshot_store: &Arc<Mutex<Option<DomSnapshot>>>,
    ) {
        let mut ws = match accept(stream) {
            Ok(ws) => ws,
            Err(e) => {
                eprintln!("[DevTools] WebSocket handshake failed: {e}");
                return;
            }
        };

        eprintln!("[DevTools] Client connected");
        let _ = ws.get_ref().set_nonblocking(true);

        let mut handler = CdpHandler::new();

        let _ = tx.send(DevToolsToMain::RequestSnapshot);

        loop {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    MainToDevTools::Snapshot(snap) => {
                        *snapshot_store.lock().unwrap() = Some(snap);
                    }
                    MainToDevTools::Shutdown => {
                        let _ = ws.close(None);
                        return;
                    }
                }
            }

            match ws.read() {
                Ok(Message::Text(text)) => {
                    Self::process_message(&text, &mut handler, &mut ws, snapshot_store, tx);
                }
                Ok(Message::Close(_)) => {
                    eprintln!("[DevTools] Client disconnected");
                    return;
                }
                Ok(Message::Ping(data)) => {
                    let _ = ws.send(Message::Pong(data));
                }
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    thread::sleep(std::time::Duration::from_millis(16));
                }
                Err(tungstenite::Error::ConnectionClosed) => {
                    eprintln!("[DevTools] Connection closed");
                    return;
                }
                Err(e) => {
                    if !matches!(e, tungstenite::Error::Protocol(_)) {
                        eprintln!("[DevTools] WebSocket error: {e}");
                    }
                    return;
                }
                _ => {}
            }
        }
    }

    fn process_message(
        text: &str,
        handler: &mut CdpHandler,
        ws: &mut WebSocket<TcpStream>,
        snapshot_store: &Arc<Mutex<Option<DomSnapshot>>>,
        tx: &mpsc::Sender<DevToolsToMain>,
    ) {
        let req: CdpRequest = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[DevTools] Invalid CDP message: {e}");
                return;
            }
        };

        let needs_snapshot = matches!(
            req.method.as_str(),
            "DOM.getDocument"
                | "DOM.requestChildNodes"
                | "DOM.querySelector"
                | "DOM.querySelectorAll"
                | "DOM.getOuterHTML"
                | "DOM.getBoxModel"
                | "DOM.getNodeForLocation"
                | "DOM.describeNode"
                | "CSS.getComputedStyleForNode"
                | "CSS.getMatchedStylesForNode"
                | "CSS.getInlineStylesForNode"
        );

        if needs_snapshot {
            let _ = tx.send(DevToolsToMain::RequestSnapshot);
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(200);
            loop {
                if snapshot_store.lock().unwrap().is_some() {
                    break;
                }
                if std::time::Instant::now() > deadline {
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        if req.method == "Overlay.highlightNode" {
            let node_id = req.params["nodeId"].as_i64();
            let _ = tx.send(DevToolsToMain::HighlightNode(node_id));
        }
        if req.method == "Overlay.hideHighlight" {
            let _ = tx.send(DevToolsToMain::HighlightNode(None));
        }

        let lock = snapshot_store.lock().unwrap();
        let response = if let Some(snap) = lock.as_ref() {
            handler.handle(&req, &snap.serialized_doc, &snap.layout_rects)
        } else {
            CdpResponse {
                id: req.id,
                result: json!({}),
            }
        };
        drop(lock);

        let json = serde_json::to_string(&response).unwrap_or_default();
        let _ = ws.send(Message::Text(json.into()));

        // After DOM.getDocument, fire documentUpdated so DevTools knows the tree
        if req.method == "DOM.enable" {
            let event = CdpEvent {
                method: "DOM.documentUpdated".into(),
                params: json!({}),
            };
            let json = serde_json::to_string(&event).unwrap_or_default();
            let _ = ws.send(Message::Text(json.into()));
        }
    }
}
