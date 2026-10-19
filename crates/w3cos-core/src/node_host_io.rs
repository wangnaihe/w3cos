//! Child processes, sockets and the permission hook behind `w3cos-node`'s
//! `child_process` and `net` glue (M3).
//!
//! Every entry that starts a process or opens a socket asks the process-wide
//! [permission policy](set_permission_policy) first and throws Node's
//! `ERR_ACCESS_DENIED` when it refuses, so an embedder (the w3cos shell, the
//! AI bridge) can route `child_process` and network access through its own
//! capability gate and receipts. Without an installed policy everything is
//! allowed except the scopes listed in `W3COS_NODE_DENY` (`child`, `net`,
//! `worker`, or `*`): an app the user launched is trusted like `node`.
//!
//! The host functions never block the event loop: pipe readers and stdin
//! writers run on threads, sockets are non-blocking, and the glue polls
//! (`cpPoll`, `netAccept`, `netRead`) from a timer while handles are open.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use crate::Value;
use crate::node_host::{arg, data_bytes, define, node_error};

// --- permissions ---------------------------------------------------------------

/// Decides `(scope, resource)`: scope is `child`, `net` or `worker`; the
/// resource names the program, address or module.
pub type PermissionPolicy = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

fn policy_slot() -> &'static RwLock<Option<PermissionPolicy>> {
    static POLICY: OnceLock<RwLock<Option<PermissionPolicy>>> = OnceLock::new();
    POLICY.get_or_init(|| RwLock::new(None))
}

/// Install (or with `None` remove) the process-wide Node permission policy.
pub fn set_permission_policy(policy: Option<PermissionPolicy>) {
    *policy_slot().write().unwrap_or_else(|e| e.into_inner()) = policy;
}

/// Whether `scope` may be used for `resource` under the current policy.
pub fn permission_granted(scope: &str, resource: &str) -> bool {
    if let Some(policy) = policy_slot()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return policy(scope, resource);
    }
    match std::env::var("W3COS_NODE_DENY") {
        Ok(list) => !list
            .split(',')
            .map(str::trim)
            .any(|denied| denied == scope || denied == "*"),
        Err(_) => true,
    }
}

fn permission_name(scope: &str) -> &'static str {
    match scope {
        "child" => "ChildProcess",
        "worker" => "WorkerThreads",
        "net" => "Net",
        _ => "FileSystem",
    }
}

/// Throw Node's permission-model error unless `scope` is granted.
pub(crate) fn require_permission(scope: &str, resource: &str) {
    if permission_granted(scope, resource) {
        return;
    }
    let error = crate::builtins::error_instance(
        "Error",
        vec![Value::from("Access to this API has been restricted")],
    );
    error.set_property("code", Value::from("ERR_ACCESS_DENIED"));
    error.set_property("permission", Value::from(permission_name(scope)));
    error.set_property("resource", Value::from(resource));
    crate::throw_value(error)
}

// --- helpers -------------------------------------------------------------------

fn string_list(value: &Value) -> Vec<String> {
    if value.is_nullish() {
        return Vec::new();
    }
    let length = value.get_property("length").to_number();
    if !length.is_finite() || length < 0.0 {
        return Vec::new();
    }
    (0..length as usize)
        .map(|i| value.get_property(&i.to_string()).to_js_string())
        .collect()
}

fn optional_string(value: &Value) -> Option<String> {
    (!value.is_nullish()).then(|| value.to_js_string())
}

fn bytes_value(bytes: Vec<u8>) -> Value {
    crate::binary::array_buffer_value(bytes)
}

fn object(fields: Vec<(&str, Value)>) -> Value {
    Value::object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<HashMap<_, _>>(),
    )
}

// --- child processes -------------------------------------------------------------

#[derive(Default)]
struct Pipe {
    chunks: Vec<Vec<u8>>,
    done: bool,
}

struct Proc {
    child: Child,
    stdin: Option<Sender<Vec<u8>>>,
    stdout: Option<Arc<Mutex<Pipe>>>,
    stderr: Option<Arc<Mutex<Pipe>>>,
    status: Option<(Option<i32>, Option<i32>)>,
}

thread_local! {
    static PROCS: RefCell<HashMap<i64, Proc>> = RefCell::new(HashMap::new());
    static SOCKETS: RefCell<HashMap<i64, Socket>> = RefCell::new(HashMap::new());
    static LISTENERS: RefCell<HashMap<i64, Listener>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<i64> = const { Cell::new(1) };
}

fn next_id() -> i64 {
    NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    })
}

fn stdio(kind: &str) -> Stdio {
    match kind {
        "inherit" => Stdio::inherit(),
        "ignore" => Stdio::null(),
        _ => Stdio::piped(),
    }
}

/// `[file, args, cwd, envPairs | null, [stdin, stdout, stderr]]` from the glue.
fn build_command(args: &[Value]) -> (String, Vec<String>, Command) {
    let file = arg(args, 0).to_js_string();
    let argv = string_list(&arg(args, 1));
    let mut command = Command::new(&file);
    command.args(&argv);
    if let Some(cwd) = optional_string(&arg(args, 2)) {
        command.current_dir(cwd);
    }
    let env = arg(args, 3);
    if !env.is_nullish() {
        command.env_clear();
        for pair in string_list(&env) {
            if let Some((key, value)) = pair.split_once('=') {
                command.env(key, value);
            }
        }
    }
    let modes = string_list(&arg(args, 4));
    let mode = |i: usize| modes.get(i).map(String::as_str).unwrap_or("pipe");
    command
        .stdin(stdio(mode(0)))
        .stdout(stdio(mode(1)))
        .stderr(stdio(mode(2)));
    (file, argv, command)
}

fn spawn_error(error: &io::Error, file: &str, argv: &[String]) -> Value {
    let value = node_error(error, &format!("spawn {file}"), Some(file), None);
    value.set_property(
        "spawnargs",
        Value::array(argv.iter().map(|a| Value::from(a.as_str())).collect()),
    );
    value
}

fn reader(mut source: impl Read + Send + 'static) -> Arc<Mutex<Pipe>> {
    let pipe = Arc::new(Mutex::new(Pipe::default()));
    let sink = pipe.clone();
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 65536];
        loop {
            match source.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.lock().unwrap().chunks.push(buffer[..n].to_vec()),
            }
        }
        sink.lock().unwrap().done = true;
    });
    pipe
}

fn writer(mut sink: impl Write + Send + 'static) -> Sender<Vec<u8>> {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for chunk in receiver {
            if sink.write_all(&chunk).is_err() {
                break;
            }
        }
    });
    sender
}

fn exit_status(status: std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (status.code(), status.signal())
    }
    #[cfg(not(unix))]
    {
        (status.code(), None)
    }
}

fn status_fields(status: Option<(Option<i32>, Option<i32>)>) -> (Value, Value) {
    let (code, signal) = status.unwrap_or((None, None));
    (
        code.map_or(Value::Null, |c| Value::Number(f64::from(c))),
        signal.map_or(Value::Null, |s| Value::Number(f64::from(s))),
    )
}

fn drain(pipe: &Option<Arc<Mutex<Pipe>>>) -> (Value, bool) {
    match pipe {
        None => (Value::array(Vec::new()), true),
        Some(pipe) => {
            let mut pipe = pipe.lock().unwrap();
            let chunks = std::mem::take(&mut pipe.chunks);
            (
                Value::array(chunks.into_iter().map(bytes_value).collect()),
                pipe.done,
            )
        }
    }
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, signal) == 0 }
}

pub(crate) fn install(host: &Value) {
    define(host, "permissionHas", |args| {
        let scope = arg(&args, 0).to_js_string();
        let resource = arg(&args, 1);
        let resource = if resource.is_nullish() {
            String::new()
        } else {
            resource.to_js_string()
        };
        Value::Bool(permission_granted(&scope, &resource))
    });

    // cpSpawn(file, args, cwd, envPairs, stdio) -> { id, pid }
    define(host, "cpSpawn", |args| {
        let (file, argv, mut command) = build_command(&args);
        require_permission("child", &file);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => crate::throw_value(spawn_error(&error, &file, &argv)),
        };
        let pid = child.id();
        let proc = Proc {
            stdin: child.stdin.take().map(writer),
            stdout: child.stdout.take().map(reader),
            stderr: child.stderr.take().map(reader),
            child,
            status: None,
        };
        let id = next_id();
        PROCS.with(|procs| procs.borrow_mut().insert(id, proc));
        object(vec![
            ("id", Value::Number(id as f64)),
            ("pid", Value::Number(f64::from(pid))),
        ])
    });

    // cpPoll(id) -> { stdout, stderr, stdoutDone, stderrDone, exited, code, signal }
    define(host, "cpPoll", |args| {
        let id = arg(&args, 0).to_number() as i64;
        PROCS.with(|procs| {
            let mut procs = procs.borrow_mut();
            let Some(proc) = procs.get_mut(&id) else {
                return Value::Undefined;
            };
            if proc.status.is_none()
                && let Ok(Some(status)) = proc.child.try_wait()
            {
                proc.status = Some(exit_status(status));
            }
            let (stdout, stdout_done) = drain(&proc.stdout);
            let (stderr, stderr_done) = drain(&proc.stderr);
            let (code, signal) = status_fields(proc.status);
            object(vec![
                ("stdout", stdout),
                ("stderr", stderr),
                ("stdoutDone", Value::Bool(stdout_done)),
                ("stderrDone", Value::Bool(stderr_done)),
                ("exited", Value::Bool(proc.status.is_some())),
                ("code", code),
                ("signal", signal),
            ])
        })
    });

    define(host, "cpWrite", |args| {
        let id = arg(&args, 0).to_number() as i64;
        let data = data_bytes(&arg(&args, 1));
        let sent = PROCS.with(|procs| {
            procs
                .borrow()
                .get(&id)
                .and_then(|proc| proc.stdin.as_ref())
                .is_some_and(|stdin| stdin.send(data).is_ok())
        });
        Value::Bool(sent)
    });

    define(host, "cpEndStdin", |args| {
        let id = arg(&args, 0).to_number() as i64;
        PROCS.with(|procs| {
            if let Some(proc) = procs.borrow_mut().get_mut(&id) {
                proc.stdin = None;
            }
        });
        Value::Undefined
    });

    define(host, "cpKill", |args| {
        let id = arg(&args, 0).to_number() as i64;
        let signal = arg(&args, 1).to_number() as i32;
        let killed = PROCS.with(|procs| {
            let mut procs = procs.borrow_mut();
            let Some(proc) = procs.get_mut(&id) else {
                return false;
            };
            if proc.status.is_some() {
                return false;
            }
            #[cfg(unix)]
            {
                let _ = &proc.child;
                send_signal(proc.child.id(), signal)
            }
            #[cfg(not(unix))]
            {
                let _ = signal;
                proc.child.kill().is_ok()
            }
        });
        Value::Bool(killed)
    });

    define(host, "cpRelease", |args| {
        let id = arg(&args, 0).to_number() as i64;
        PROCS.with(|procs| procs.borrow_mut().remove(&id));
        Value::Undefined
    });

    // cpSpawnSync(file, args, cwd, envPairs, stdio, input, timeoutMs, killSignal)
    //   -> { pid, status, signal, stdout, stderr, timedOut }
    define(host, "cpSpawnSync", |args| {
        let (file, argv, mut command) = build_command(&args);
        require_permission("child", &file);
        let input = arg(&args, 5);
        let timeout = arg(&args, 6).to_number();
        let kill_signal = arg(&args, 7).to_number();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => crate::throw_value(spawn_error(&error, &file, &argv)),
        };
        let pid = child.id();
        if let Some(stdin) = child.stdin.take() {
            let data = if input.is_nullish() {
                Vec::new()
            } else {
                data_bytes(&input)
            };
            let sender = writer(stdin);
            let _ = sender.send(data);
        }
        let stdout = child.stdout.take().map(reader);
        let stderr = child.stderr.take().map(reader);
        let deadline = (timeout.is_finite() && timeout > 0.0)
            .then(|| Instant::now() + Duration::from_millis(timeout as u64));
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(exit_status(status)),
                Ok(None) => {}
                Err(_) => break None,
            }
            if !timed_out && deadline.is_some_and(|at| Instant::now() >= at) {
                timed_out = true;
                #[cfg(unix)]
                {
                    let signal = if kill_signal.is_finite() && kill_signal > 0.0 {
                        kill_signal as i32
                    } else {
                        15
                    };
                    send_signal(pid, signal);
                }
                #[cfg(not(unix))]
                {
                    let _ = kill_signal;
                    let _ = child.kill();
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let collect = |pipe: Option<Arc<Mutex<Pipe>>>| match pipe {
            None => Value::Null,
            Some(pipe) => loop {
                let guard = pipe.lock().unwrap();
                if guard.done {
                    break bytes_value(guard.chunks.concat());
                }
                drop(guard);
                std::thread::sleep(Duration::from_millis(1));
            },
        };
        let stdout = collect(stdout);
        let stderr = collect(stderr);
        let (code, signal) = status_fields(status);
        object(vec![
            ("pid", Value::Number(f64::from(pid))),
            ("status", code),
            ("signal", signal),
            ("stdout", stdout),
            ("stderr", stderr),
            ("timedOut", Value::Bool(timed_out)),
        ])
    });

    install_net(host);
}

// --- sockets ---------------------------------------------------------------------

enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buffer),
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buffer),
        }
    }
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(data),
            #[cfg(unix)]
            Stream::Unix(s) => s.write(data),
        }
    }
    fn shutdown_write(&self) {
        let _ = match self {
            Stream::Tcp(s) => s.shutdown(Shutdown::Write),
            #[cfg(unix)]
            Stream::Unix(s) => s.shutdown(Shutdown::Write),
        };
    }
    fn set_nonblocking(&self) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_nonblocking(true),
            #[cfg(unix)]
            Stream::Unix(s) => s.set_nonblocking(true),
        }
    }
    fn addresses(&self) -> (Value, Value) {
        match self {
            Stream::Tcp(s) => (
                s.local_addr().map_or(Value::Null, |a| address_value(&a)),
                s.peer_addr().map_or(Value::Null, |a| address_value(&a)),
            ),
            #[cfg(unix)]
            Stream::Unix(_) => (Value::Null, Value::Null),
        }
    }
}

struct Socket {
    stream: Stream,
    pending: Vec<u8>,
    end_requested: bool,
    ended: bool,
}

impl Socket {
    /// Write what the kernel takes; finish a requested `end()` once flushed.
    fn flush(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() {
            match self.stream.write(&self.pending) {
                Ok(0) => break,
                Ok(n) => {
                    self.pending.drain(..n);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        if self.pending.is_empty() && self.end_requested && !self.ended {
            self.ended = true;
            self.stream.shutdown_write();
        }
        Ok(())
    }
}

enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(UnixListener, String),
}

fn address_value(address: &std::net::SocketAddr) -> Value {
    object(vec![
        ("address", Value::from(address.ip().to_string().as_str())),
        (
            "family",
            Value::from(if address.is_ipv4() { "IPv4" } else { "IPv6" }),
        ),
        ("port", Value::Number(f64::from(address.port()))),
    ])
}

fn net_error(error: &io::Error, syscall: &str, address: &str, port: Option<u16>) -> Value {
    let value = node_error(error, syscall, None, None);
    let message = match port {
        Some(port) => format!(
            "{} {address}:{port}",
            value.get_property("message").to_js_string()
        ),
        None => format!("{} {address}", value.get_property("message").to_js_string()),
    };
    value.set_property("message", Value::from(message.as_str()));
    value.set_property("address", Value::from(address));
    if let Some(port) = port {
        value.set_property("port", Value::Number(f64::from(port)));
    }
    value
}

fn add_socket(stream: Stream) -> io::Result<i64> {
    stream.set_nonblocking()?;
    let id = next_id();
    SOCKETS.with(|sockets| {
        sockets.borrow_mut().insert(
            id,
            Socket {
                stream,
                pending: Vec::new(),
                end_requested: false,
                ended: false,
            },
        )
    });
    Ok(id)
}

fn resolve(host: &str, port: u16) -> io::Result<std::net::SocketAddr> {
    let host = if host.is_empty() || host == "localhost" {
        "127.0.0.1"
    } else {
        host
    };
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "address not found"))
}

fn install_net(host: &Value) {
    // netListen(path | null, port, host) -> id
    define(host, "netListen", |args| {
        let path = optional_string(&arg(&args, 0));
        let port = arg(&args, 1).to_number();
        let port = if port.is_finite() { port as u16 } else { 0 };
        let host_name = optional_string(&arg(&args, 2)).unwrap_or_default();
        let resource = path
            .clone()
            .unwrap_or_else(|| format!("{host_name}:{port}"));
        require_permission("net", &resource);
        let listener = match &path {
            #[cfg(unix)]
            Some(path) => UnixListener::bind(path)
                .and_then(|l| l.set_nonblocking(true).map(|_| l))
                .map(|l| Listener::Unix(l, path.clone()))
                .map_err(|e| net_error(&e, "listen", path, None)),
            #[cfg(not(unix))]
            Some(path) => Err(net_error(
                &io::Error::from(io::ErrorKind::Unsupported),
                "listen",
                path,
                None,
            )),
            None => {
                let bind_host = if host_name.is_empty() {
                    "0.0.0.0"
                } else {
                    host_name.as_str()
                };
                resolve(bind_host, port)
                    .and_then(TcpListener::bind)
                    .and_then(|l| l.set_nonblocking(true).map(|_| l))
                    .map(Listener::Tcp)
                    .map_err(|e| net_error(&e, "listen", bind_host, Some(port)))
            }
        };
        match listener {
            Ok(listener) => {
                let id = next_id();
                LISTENERS.with(|l| l.borrow_mut().insert(id, listener));
                Value::Number(id as f64)
            }
            Err(error) => crate::throw_value(error),
        }
    });

    define(host, "netAddress", |args| {
        let id = arg(&args, 0).to_number() as i64;
        LISTENERS.with(|listeners| match listeners.borrow().get(&id) {
            Some(Listener::Tcp(l)) => l.local_addr().map_or(Value::Null, |a| address_value(&a)),
            #[cfg(unix)]
            Some(Listener::Unix(_, path)) => Value::from(path.as_str()),
            None => Value::Null,
        })
    });

    // netAccept(id) -> socket id | undefined
    define(host, "netAccept", |args| {
        let id = arg(&args, 0).to_number() as i64;
        let accepted = LISTENERS.with(|listeners| match listeners.borrow().get(&id) {
            Some(Listener::Tcp(l)) => l.accept().map(|(s, _)| Stream::Tcp(s)),
            #[cfg(unix)]
            Some(Listener::Unix(l, _)) => l.accept().map(|(s, _)| Stream::Unix(s)),
            None => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        });
        match accepted.and_then(add_socket) {
            Ok(id) => Value::Number(id as f64),
            Err(_) => Value::Undefined,
        }
    });

    // netConnect(path | null, port, host) -> socket id
    define(host, "netConnect", |args| {
        let path = optional_string(&arg(&args, 0));
        let port = arg(&args, 1).to_number();
        let port = if port.is_finite() { port as u16 } else { 0 };
        let host_name = optional_string(&arg(&args, 2)).unwrap_or_else(|| "localhost".into());
        let resource = path
            .clone()
            .unwrap_or_else(|| format!("{host_name}:{port}"));
        require_permission("net", &resource);
        let connected = match &path {
            #[cfg(unix)]
            Some(path) => UnixStream::connect(path)
                .map(Stream::Unix)
                .map_err(|e| net_error(&e, "connect", path, None)),
            #[cfg(not(unix))]
            Some(path) => Err(net_error(
                &io::Error::from(io::ErrorKind::Unsupported),
                "connect",
                path,
                None,
            )),
            None => {
                let address = resolve(&host_name, port);
                let shown = address
                    .as_ref()
                    .map_or(host_name.clone(), |a| a.ip().to_string());
                address
                    .and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(10)))
                    .map(|s| {
                        let _ = s.set_nodelay(true);
                        Stream::Tcp(s)
                    })
                    .map_err(|e| net_error(&e, "connect", &shown, Some(port)))
            }
        };
        match connected {
            Ok(stream) => match add_socket(stream) {
                Ok(id) => Value::Number(id as f64),
                Err(error) => crate::throw_value(net_error(&error, "connect", &resource, None)),
            },
            Err(error) => crate::throw_value(error),
        }
    });

    define(host, "netSocketAddresses", |args| {
        let id = arg(&args, 0).to_number() as i64;
        SOCKETS.with(|sockets| match sockets.borrow().get(&id) {
            Some(socket) => {
                let (local, remote) = socket.stream.addresses();
                object(vec![("local", local), ("remote", remote)])
            }
            None => Value::Null,
        })
    });

    // netRead(id) -> ArrayBuffer | null (EOF) | undefined (nothing yet); throws on reset.
    define(host, "netRead", |args| {
        let id = arg(&args, 0).to_number() as i64;
        let result = SOCKETS.with(|sockets| {
            let mut sockets = sockets.borrow_mut();
            let Some(socket) = sockets.get_mut(&id) else {
                return Ok(Value::Null);
            };
            socket.flush()?;
            let mut buffer = vec![0u8; 65536];
            match socket.stream.read(&mut buffer) {
                Ok(0) => Ok(Value::Null),
                Ok(n) => {
                    buffer.truncate(n);
                    Ok(bytes_value(buffer))
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    Ok(Value::Undefined)
                }
                Err(error) => Err(error),
            }
        });
        result.unwrap_or_else(|error| crate::throw_value(node_error(&error, "read", None, None)))
    });

    // netWrite(id, data) -> bytes still queued; throws on a broken connection.
    define(host, "netWrite", |args| {
        let id = arg(&args, 0).to_number() as i64;
        let data = data_bytes(&arg(&args, 1));
        let result = SOCKETS.with(|sockets| {
            let mut sockets = sockets.borrow_mut();
            let Some(socket) = sockets.get_mut(&id) else {
                return Err(io::Error::from_raw_os_error(32));
            };
            socket.pending.extend_from_slice(&data);
            socket.flush().map(|_| socket.pending.len())
        });
        match result {
            Ok(queued) => Value::Number(queued as f64),
            Err(error) => crate::throw_value(node_error(&error, "write", None, None)),
        }
    });

    // netPending(id) -> bytes still queued after flushing.
    define(host, "netPending", |args| {
        let id = arg(&args, 0).to_number() as i64;
        SOCKETS.with(|sockets| {
            let mut sockets = sockets.borrow_mut();
            match sockets.get_mut(&id) {
                Some(socket) => {
                    let _ = socket.flush();
                    Value::Number(socket.pending.len() as f64)
                }
                None => Value::Number(0.0),
            }
        })
    });

    define(host, "netEnd", |args| {
        let id = arg(&args, 0).to_number() as i64;
        SOCKETS.with(|sockets| {
            if let Some(socket) = sockets.borrow_mut().get_mut(&id) {
                socket.end_requested = true;
                let _ = socket.flush();
            }
        });
        Value::Undefined
    });

    define(host, "netClose", |args| {
        let id = arg(&args, 0).to_number() as i64;
        SOCKETS.with(|sockets| sockets.borrow_mut().remove(&id));
        let listener = LISTENERS.with(|listeners| listeners.borrow_mut().remove(&id));
        #[cfg(unix)]
        if let Some(Listener::Unix(_, path)) = listener {
            // Node unlinks the socket file when the server closes.
            let _ = std::fs::remove_file(path);
        }
        #[cfg(not(unix))]
        drop(listener);
        Value::Undefined
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_overrides_the_env_deny_list() {
        set_permission_policy(Some(Arc::new(|scope, resource| {
            !(scope == "child" && resource == "rm")
        })));
        assert!(!permission_granted("child", "rm"));
        assert!(permission_granted("child", "ls"));
        assert!(permission_granted("net", "/tmp/x.sock"));
        set_permission_policy(None);
        assert!(permission_granted("child", "rm"));
    }
}
