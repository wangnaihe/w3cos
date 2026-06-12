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

// ---------------------------------------------------------------------------
// Shared Memory (w3cos.ipc.shm) — Unix only
// ---------------------------------------------------------------------------

/// A POSIX shared memory segment.
///
/// Creates or opens a named shared memory object. The segment is unmapped
/// and unlinked when this struct is dropped (if `owner` is true).
#[cfg(unix)]
pub struct SharedMemory {
    name: String,
    ptr: *mut libc::c_void,
    size: usize,
    owner: bool,
}

#[cfg(unix)]
unsafe impl Send for SharedMemory {}
#[cfg(unix)]
unsafe impl Sync for SharedMemory {}

#[cfg(unix)]
impl SharedMemory {
    /// Create a new named shared memory segment of `size` bytes.
    pub fn create(name: &str, size: usize) -> Result<Self, String> {
        use libc::{ftruncate, mmap, shm_open};
        use libc::{MAP_SHARED, O_CREAT, O_RDWR, PROT_READ, PROT_WRITE, S_IRUSR, S_IWUSR};
        use std::ffi::CString;

        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let fd = unsafe { shm_open(cname.as_ptr(), O_CREAT | O_RDWR, (S_IRUSR | S_IWUSR) as libc::c_uint) };
        if fd < 0 {
            return Err(format!("shm_open failed: {}", std::io::Error::last_os_error()));
        }
        if unsafe { ftruncate(fd, size as libc::off_t) } < 0 {
            unsafe { libc::close(fd) };
            return Err(format!("ftruncate failed: {}", std::io::Error::last_os_error()));
        }
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };
        unsafe { libc::close(fd) };
        if ptr == libc::MAP_FAILED {
            return Err(format!("mmap failed: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { name: name.to_string(), ptr, size, owner: true })
    }

    /// Open an existing named shared memory segment.
    pub fn open(name: &str, size: usize) -> Result<Self, String> {
        use libc::{mmap, shm_open};
        use libc::{MAP_SHARED, O_RDWR, PROT_READ, PROT_WRITE};
        use std::ffi::CString;

        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let fd = unsafe { shm_open(cname.as_ptr(), O_RDWR, 0) };
        if fd < 0 {
            return Err(format!("shm_open failed: {}", std::io::Error::last_os_error()));
        }
        let ptr = unsafe {
            mmap(std::ptr::null_mut(), size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)
        };
        unsafe { libc::close(fd) };
        if ptr == libc::MAP_FAILED {
            return Err(format!("mmap failed: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { name: name.to_string(), ptr, size, owner: false })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Write bytes into the shared memory at `offset`.
    pub fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), String> {
        if offset + data.len() > self.size {
            return Err("write out of bounds".to_string());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (self.ptr as *mut u8).add(offset),
                data.len(),
            );
        }
        Ok(())
    }

    /// Read bytes from the shared memory at `offset`.
    pub fn read(&self, offset: usize, len: usize) -> Result<Vec<u8>, String> {
        if offset + len > self.size {
            return Err("read out of bounds".to_string());
        }
        let mut buf = vec![0u8; len];
        unsafe {
            std::ptr::copy_nonoverlapping(
                (self.ptr as *const u8).add(offset),
                buf.as_mut_ptr(),
                len,
            );
        }
        Ok(buf)
    }

    /// Get a raw mutable pointer to the shared memory region.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr as *mut u8
    }

    /// Get a raw const pointer to the shared memory region.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr as *const u8
    }
}

#[cfg(unix)]
impl Drop for SharedMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr, self.size);
            if self.owner {
                let cname = std::ffi::CString::new(self.name.as_str()).unwrap();
                libc::shm_unlink(cname.as_ptr());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Named Semaphore (w3cos.ipc.semaphore) — Unix only
// ---------------------------------------------------------------------------

/// A POSIX named semaphore for cross-process synchronization.
#[cfg(unix)]
pub struct NamedSemaphore {
    name: String,
    sem: *mut libc::sem_t,
    owner: bool,
}

#[cfg(unix)]
unsafe impl Send for NamedSemaphore {}
#[cfg(unix)]
unsafe impl Sync for NamedSemaphore {}

#[cfg(unix)]
impl NamedSemaphore {
    /// Create a new named semaphore with an initial value.
    pub fn create(name: &str, initial: u32) -> Result<Self, String> {
        use libc::{sem_open, O_CREAT, O_EXCL, S_IRUSR, S_IWUSR};
        use std::ffi::CString;

        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let sem = unsafe {
            sem_open(
                cname.as_ptr(),
                O_CREAT | O_EXCL,
                (S_IRUSR | S_IWUSR) as libc::c_uint,
                initial,
            )
        };
        if sem == libc::SEM_FAILED {
            return Err(format!("sem_open failed: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { name: name.to_string(), sem, owner: true })
    }

    /// Open an existing named semaphore.
    pub fn open(name: &str) -> Result<Self, String> {
        use std::ffi::CString;
        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let sem = unsafe { libc::sem_open(cname.as_ptr(), 0) };
        if sem == libc::SEM_FAILED {
            return Err(format!("sem_open failed: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { name: name.to_string(), sem, owner: false })
    }

    /// Decrement (wait/lock) the semaphore. Blocks if value is 0.
    pub fn wait(&self) -> Result<(), String> {
        if unsafe { libc::sem_wait(self.sem) } == 0 {
            Ok(())
        } else {
            Err(format!("sem_wait failed: {}", std::io::Error::last_os_error()))
        }
    }

    /// Non-blocking decrement. Returns `false` if would block.
    pub fn try_wait(&self) -> bool {
        unsafe { libc::sem_trywait(self.sem) == 0 }
    }

    /// Increment (post/unlock) the semaphore.
    pub fn post(&self) -> Result<(), String> {
        if unsafe { libc::sem_post(self.sem) } == 0 {
            Ok(())
        } else {
            Err(format!("sem_post failed: {}", std::io::Error::last_os_error()))
        }
    }

    /// Get the current semaphore value.
    pub fn value(&self) -> Result<i32, String> {
        #[cfg(target_os = "linux")]
        {
            let mut val: libc::c_int = 0;
            if unsafe { libc::sem_getvalue(self.sem, &mut val) } == 0 {
                Ok(val)
            } else {
                Err(format!("sem_getvalue failed: {}", std::io::Error::last_os_error()))
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // sem_getvalue is not reliably available on macOS; return -1 as unsupported.
            Err("sem_getvalue not supported on this platform".to_string())
        }
    }
}

#[cfg(unix)]
impl Drop for NamedSemaphore {
    fn drop(&mut self) {
        unsafe {
            libc::sem_close(self.sem);
            if self.owner {
                let cname = std::ffi::CString::new(self.name.as_str()).unwrap();
                libc::sem_unlink(cname.as_ptr());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Message Queue (w3cos.ipc.mqueue) — Linux only
// ---------------------------------------------------------------------------

/// A POSIX message queue for typed inter-process messaging.
///
/// Available on Linux only (macOS removed mqueue support).
#[cfg(target_os = "linux")]
pub struct MessageQueue {
    name: String,
    mqd: libc::mqd_t,
    owner: bool,
}

#[cfg(target_os = "linux")]
unsafe impl Send for MessageQueue {}
#[cfg(target_os = "linux")]
unsafe impl Sync for MessageQueue {}

#[cfg(target_os = "linux")]
impl MessageQueue {
    /// Create a new message queue.
    pub fn create(name: &str, max_msgs: i64, max_msg_size: i64) -> Result<Self, String> {
        use libc::{mq_open, O_CREAT, O_RDWR, S_IRUSR, S_IWUSR};
        use std::ffi::CString;

        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let attr = libc::mq_attr {
            mq_flags: 0,
            mq_maxmsg: max_msgs,
            mq_msgsize: max_msg_size,
            mq_curmsgs: 0,
            __pad: [0; 4],
        };
        let mqd = unsafe {
            mq_open(
                cname.as_ptr(),
                O_CREAT | O_RDWR,
                (S_IRUSR | S_IWUSR) as libc::c_uint,
                &attr as *const libc::mq_attr,
            )
        };
        if mqd == -1 as libc::mqd_t {
            return Err(format!("mq_open failed: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { name: name.to_string(), mqd, owner: true })
    }

    /// Open an existing message queue.
    pub fn open(name: &str) -> Result<Self, String> {
        use libc::{mq_open, O_RDWR};
        use std::ffi::CString;

        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let mqd = unsafe { mq_open(cname.as_ptr(), O_RDWR) };
        if mqd == -1 as libc::mqd_t {
            return Err(format!("mq_open failed: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { name: name.to_string(), mqd, owner: false })
    }

    /// Send a message with the given priority (0 = lowest).
    pub fn send(&self, data: &[u8], priority: u32) -> Result<(), String> {
        let ret = unsafe {
            libc::mq_send(self.mqd, data.as_ptr() as *const libc::c_char, data.len(), priority)
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(format!("mq_send failed: {}", std::io::Error::last_os_error()))
        }
    }

    /// Receive the next message. Blocks until a message is available.
    pub fn recv(&self, max_size: usize) -> Result<(Vec<u8>, u32), String> {
        let mut buf = vec![0u8; max_size];
        let mut priority: u32 = 0;
        let n = unsafe {
            libc::mq_receive(
                self.mqd,
                buf.as_mut_ptr() as *mut libc::c_char,
                max_size,
                &mut priority,
            )
        };
        if n < 0 {
            return Err(format!("mq_receive failed: {}", std::io::Error::last_os_error()));
        }
        buf.truncate(n as usize);
        Ok((buf, priority))
    }

    /// Get current queue attributes (current message count, etc.).
    pub fn attributes(&self) -> Result<(i64, i64, i64), String> {
        let mut attr = libc::mq_attr {
            mq_flags: 0,
            mq_maxmsg: 0,
            mq_msgsize: 0,
            mq_curmsgs: 0,
            __pad: [0; 4],
        };
        if unsafe { libc::mq_getattr(self.mqd, &mut attr) } == 0 {
            Ok((attr.mq_maxmsg, attr.mq_msgsize, attr.mq_curmsgs))
        } else {
            Err(format!("mq_getattr failed: {}", std::io::Error::last_os_error()))
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for MessageQueue {
    fn drop(&mut self) {
        unsafe {
            libc::mq_close(self.mqd);
            if self.owner {
                let cname = std::ffi::CString::new(self.name.as_str()).unwrap();
                libc::mq_unlink(cname.as_ptr());
            }
        }
    }
}
