//! `w3cos.ipc` — typed inter-process message bus.
//!
//! Models Electron's `ipcMain` / `ipcRenderer` pair using a length-prefixed
//! JSON protocol over Unix Domain Sockets (Linux/macOS) or a TCP loopback
//! socket (Windows / sandboxed environments). Each message is `{channel,
//! payload}` so multiple subsystems can share the same bus.
//!
//! ## Servers
//!
//! ```ignore
//! let server = IpcServer::bind("/tmp/w3cos-app.sock")?;
//! while let Some(msg) = server.recv() {
//!     if msg.channel == "ping" { server.broadcast("pong", json!({"ok": true})); }
//! }
//! ```
//!
//! ## Clients
//!
//! ```ignore
//! let client = IpcClient::connect("/tmp/w3cos-app.sock")?;
//! client.send("ping", json!({}))?;
//! while let Some(reply) = client.try_recv() { /* … */ }
//! ```
//!
//! Both halves are non-blocking from the application's perspective — IO is
//! handled on dedicated threads and surfaced through a `Receiver`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub channel: String,
    pub payload: serde_json::Value,
}

impl IpcMessage {
    pub fn new(channel: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            channel: channel.into(),
            payload,
        }
    }
}

/// Client-side handle. Hides the worker thread + outgoing queue.
pub struct IpcClient {
    out_tx: mpsc::Sender<IpcMessage>,
    in_rx: Arc<Mutex<mpsc::Receiver<IpcMessage>>>,
}

impl IpcClient {
    /// Connect to a Unix socket path or `tcp://host:port` URI.
    pub fn connect(endpoint: impl AsRef<str>) -> std::io::Result<Self> {
        let endpoint = endpoint.as_ref();
        if let Some(rest) = endpoint.strip_prefix("tcp://") {
            let stream = TcpStream::connect(rest)?;
            Ok(Self::from_tcp(stream))
        } else {
            #[cfg(unix)]
            {
                let stream = UnixStream::connect(Path::new(endpoint))?;
                Ok(Self::from_unix(stream))
            }
            #[cfg(not(unix))]
            {
                let stream = TcpStream::connect(endpoint)?;
                Ok(Self::from_tcp(stream))
            }
        }
    }

    #[cfg(unix)]
    fn from_unix(stream: UnixStream) -> Self {
        let writer = stream.try_clone().expect("clone unix stream");
        Self::wire(stream, writer)
    }

    fn from_tcp(stream: TcpStream) -> Self {
        let writer = stream.try_clone().expect("clone tcp stream");
        Self::wire(stream, writer)
    }

    fn wire<R, W>(reader: R, mut writer: W) -> Self
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel::<IpcMessage>();
        let (in_tx, in_rx) = mpsc::channel::<IpcMessage>();

        // Writer thread.
        thread::Builder::new()
            .name("w3cos-ipc-write".into())
            .spawn(move || {
                while let Ok(msg) = out_rx.recv() {
                    if write_message(&mut writer, &msg).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn ipc writer");

        // Reader thread.
        let mut reader = reader;
        thread::Builder::new()
            .name("w3cos-ipc-read".into())
            .spawn(move || loop {
                match read_message(&mut reader) {
                    Ok(msg) => {
                        if in_tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            })
            .expect("spawn ipc reader");

        Self {
            out_tx,
            in_rx: Arc::new(Mutex::new(in_rx)),
        }
    }

    /// Send a typed event to the peer.
    pub fn send(&self, channel: impl Into<String>, payload: serde_json::Value) -> Result<(), String> {
        self.out_tx
            .send(IpcMessage::new(channel, payload))
            .map_err(|e| e.to_string())
    }

    /// Non-blocking receive — `None` when nothing is queued.
    pub fn try_recv(&self) -> Option<IpcMessage> {
        self.in_rx.lock().ok()?.try_recv().ok()
    }

    /// Blocking receive — preferred from worker code.
    pub fn recv(&self) -> Option<IpcMessage> {
        self.in_rx.lock().ok()?.recv().ok()
    }

    /// Drain everything pending right now.
    pub fn drain(&self) -> Vec<IpcMessage> {
        let guard = match self.in_rx.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        while let Ok(msg) = guard.try_recv() {
            out.push(msg);
        }
        out
    }
}

impl Clone for IpcClient {
    fn clone(&self) -> Self {
        Self {
            out_tx: self.out_tx.clone(),
            in_rx: Arc::clone(&self.in_rx),
        }
    }
}

/// Identifies an IPC peer for [`IpcServer::send_to`].
pub type PeerId = u64;

/// Server-side multi-client message bus.
pub struct IpcServer {
    inbox_rx: mpsc::Receiver<(PeerId, IpcMessage)>,
    peers: Arc<Mutex<HashMap<PeerId, mpsc::Sender<IpcMessage>>>>,
    endpoint: String,
}

impl IpcServer {
    /// Bind a Unix socket (`/tmp/foo.sock`) or `tcp://0.0.0.0:1234` endpoint.
    pub fn bind(endpoint: impl Into<String>) -> std::io::Result<Self> {
        let endpoint = endpoint.into();
        let peers: Arc<Mutex<HashMap<PeerId, mpsc::Sender<IpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let next_id: Arc<Mutex<PeerId>> = Arc::new(Mutex::new(1));
        let (inbox_tx, inbox_rx) = mpsc::channel();

        if let Some(rest) = endpoint.strip_prefix("tcp://") {
            let addr: SocketAddr = rest
                .parse()
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad tcp addr"))?;
            let listener = TcpListener::bind(addr)?;
            spawn_tcp_acceptor(listener, peers.clone(), next_id, inbox_tx);
        } else {
            #[cfg(unix)]
            {
                let _ = std::fs::remove_file(&endpoint);
                let listener = UnixListener::bind(Path::new(&endpoint))?;
                spawn_unix_acceptor(listener, peers.clone(), next_id, inbox_tx);
            }
            #[cfg(not(unix))]
            {
                let listener = TcpListener::bind(endpoint.as_str())?;
                spawn_tcp_acceptor(listener, peers.clone(), next_id, inbox_tx);
            }
        }

        Ok(Self {
            inbox_rx,
            peers,
            endpoint,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Drain the next message from any connected peer.
    pub fn try_recv(&self) -> Option<(PeerId, IpcMessage)> {
        self.inbox_rx.try_recv().ok()
    }

    /// Blocking receive (for worker threads or simple servers).
    pub fn recv(&self) -> Option<(PeerId, IpcMessage)> {
        self.inbox_rx.recv().ok()
    }

    /// Drain everything pending right now.
    pub fn drain(&self) -> Vec<(PeerId, IpcMessage)> {
        let mut out = Vec::new();
        while let Ok(msg) = self.inbox_rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Send to one specific peer.
    pub fn send_to(
        &self,
        peer: PeerId,
        channel: impl Into<String>,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        let msg = IpcMessage::new(channel, payload);
        let peers = self.peers.lock().map_err(|e| e.to_string())?;
        let tx = peers.get(&peer).ok_or_else(|| "peer not connected".to_string())?;
        tx.send(msg).map_err(|e| e.to_string())
    }

    /// Broadcast to every connected peer. Returns the number of peers reached.
    pub fn broadcast(&self, channel: impl Into<String>, payload: serde_json::Value) -> usize {
        let channel = channel.into();
        let peers = match self.peers.lock() {
            Ok(p) => p,
            Err(_) => return 0,
        };
        let mut count = 0;
        for tx in peers.values() {
            if tx
                .send(IpcMessage::new(channel.clone(), payload.clone()))
                .is_ok()
            {
                count += 1;
            }
        }
        count
    }

    pub fn peer_ids(&self) -> Vec<PeerId> {
        self.peers
            .lock()
            .map(|p| p.keys().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(unix)]
fn spawn_unix_acceptor(
    listener: UnixListener,
    peers: Arc<Mutex<HashMap<PeerId, mpsc::Sender<IpcMessage>>>>,
    next_id: Arc<Mutex<PeerId>>,
    inbox_tx: mpsc::Sender<(PeerId, IpcMessage)>,
) {
    thread::Builder::new()
        .name("w3cos-ipc-accept-unix".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let writer = match stream.try_clone() {
                    Ok(w) => w,
                    Err(_) => continue,
                };
                let id = next_peer_id(&next_id);
                spawn_peer_handlers(id, stream, writer, peers.clone(), inbox_tx.clone());
            }
        })
        .expect("spawn ipc unix acceptor");
}

fn spawn_tcp_acceptor(
    listener: TcpListener,
    peers: Arc<Mutex<HashMap<PeerId, mpsc::Sender<IpcMessage>>>>,
    next_id: Arc<Mutex<PeerId>>,
    inbox_tx: mpsc::Sender<(PeerId, IpcMessage)>,
) {
    thread::Builder::new()
        .name("w3cos-ipc-accept-tcp".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let writer = match stream.try_clone() {
                    Ok(w) => w,
                    Err(_) => continue,
                };
                let id = next_peer_id(&next_id);
                spawn_peer_handlers(id, stream, writer, peers.clone(), inbox_tx.clone());
            }
        })
        .expect("spawn ipc tcp acceptor");
}

fn next_peer_id(counter: &Arc<Mutex<PeerId>>) -> PeerId {
    let mut guard = counter.lock().expect("ipc next-id mutex poisoned");
    let id = *guard;
    *guard = id.wrapping_add(1);
    id
}

fn spawn_peer_handlers<R, W>(
    id: PeerId,
    reader: R,
    mut writer: W,
    peers: Arc<Mutex<HashMap<PeerId, mpsc::Sender<IpcMessage>>>>,
    inbox_tx: mpsc::Sender<(PeerId, IpcMessage)>,
) where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let (out_tx, out_rx) = mpsc::channel::<IpcMessage>();
    if let Ok(mut p) = peers.lock() {
        p.insert(id, out_tx);
    }
    let peers_for_writer = peers.clone();
    thread::Builder::new()
        .name(format!("w3cos-ipc-write-{id}"))
        .spawn(move || {
            while let Ok(msg) = out_rx.recv() {
                if write_message(&mut writer, &msg).is_err() {
                    break;
                }
            }
            if let Ok(mut p) = peers_for_writer.lock() {
                p.remove(&id);
            }
        })
        .expect("spawn ipc peer writer");

    let mut reader = reader;
    let peers_for_reader = peers;
    thread::Builder::new()
        .name(format!("w3cos-ipc-read-{id}"))
        .spawn(move || {
            loop {
                match read_message(&mut reader) {
                    Ok(msg) => {
                        if inbox_tx.send((id, msg)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Ok(mut p) = peers_for_reader.lock() {
                p.remove(&id);
            }
        })
        .expect("spawn ipc peer reader");
}

fn write_message<W: Write>(writer: &mut W, msg: &IpcMessage) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = (bytes.len() as u32).to_le_bytes();
    writer.write_all(&len)?;
    writer.write_all(&bytes)?;
    writer.flush()
}

fn read_message<R: Read>(reader: &mut R) -> std::io::Result<IpcMessage> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ipc frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn wait_for<T, F: Fn() -> Option<T>>(predicate: F) -> Option<T> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(v) = predicate() {
                return Some(v);
            }
            thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn tcp_round_trip() {
        let server = IpcServer::bind("tcp://127.0.0.1:0".to_string());
        // We need to know which port we got — bind manually instead.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        drop(server);
        let endpoint = format!("tcp://127.0.0.1:{port}");
        let server = IpcServer::bind(&endpoint).unwrap();
        thread::sleep(Duration::from_millis(50));

        let client = IpcClient::connect(&endpoint).unwrap();
        client.send("hello", json!({"n": 1})).unwrap();

        let msg = wait_for(|| server.try_recv()).expect("server should receive");
        assert_eq!(msg.1.channel, "hello");
        assert_eq!(msg.1.payload["n"], json!(1));

        server.broadcast("hi", json!({"from": "server"}));
        let reply = wait_for(|| client.try_recv()).expect("client should receive");
        assert_eq!(reply.channel, "hi");
        assert_eq!(reply.payload["from"], json!("server"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_round_trip() {
        let path = std::env::temp_dir()
            .join(format!("w3cos-ipc-{}.sock", std::process::id()));
        let endpoint = path.to_string_lossy().to_string();

        let server = IpcServer::bind(&endpoint).unwrap();
        thread::sleep(Duration::from_millis(50));
        let client = IpcClient::connect(&endpoint).unwrap();

        client.send("ping", json!({})).unwrap();
        let msg = wait_for(|| server.try_recv()).expect("ping arrives");
        assert_eq!(msg.1.channel, "ping");

        let _ = std::fs::remove_file(&path);
    }
}
