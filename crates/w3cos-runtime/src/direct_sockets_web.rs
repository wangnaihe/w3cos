//! WICG Direct Sockets: `TCPSocket`, `UDPSocket`, and `TCPServerSocket`.
//!
//! Connections use the host TCP/UDP stack. `opened` rejects `NotAllowedError`
//! until the `direct-sockets` permission is granted, and transport failures
//! reject instead of reporting a live socket.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use crate::jsdom::realm_function;
use w3cos_core::Value;

const PERMISSION: &str = "direct-sockets";

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static SOCKETS: RefCell<HashMap<u64, LiveSocket>> = RefCell::new(HashMap::new());
    static NEXT_ID: RefCell<u64> = const { RefCell::new(1) };
}

enum LiveSocket {
    Tcp {
        reader: TcpStream,
        writer: TcpStream,
    },
    Udp(UdpSocket),
    Listener(TcpListener),
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn rejected(name: &str, message: &str) -> Value {
    w3cos_core::promise::reject(vec![error(name, message)])
}

fn uint8_array(bytes: &[u8]) -> Value {
    let buffer = w3cos_core::binary::array_buffer_value(bytes.to_vec());
    w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Uint8Array"),
        vec![buffer],
    )
}

fn bytes_of(value: &Value) -> Vec<u8> {
    if value.is_object() {
        let data = value.get_property("data");
        if !data.is_undefined() {
            return bytes_of(&data);
        }
    }
    w3cos_core::binary::bytes_of(value).unwrap_or_else(|| value.to_js_string().into_bytes())
}

fn next_id() -> u64 {
    NEXT_ID.with(|next| {
        let id = *next.borrow();
        *next.borrow_mut() = id.saturating_add(1);
        id
    })
}

fn class_named(name: &'static str, constructor: Value) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        constructor.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), constructor.clone()),
            ("opened".into(), Value::Undefined),
            ("close".into(), Value::Undefined),
        ]));
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        constructor.set_property("prototype", prototype);
        classes
            .borrow_mut()
            .insert(name.to_string(), constructor.clone());
        constructor
    })
}

fn stream_pair(id: u64) -> (Value, Value) {
    let source = Value::object(HashMap::new());
    source.set_property(
        "pull",
        realm_function(crate::jsdom::realm_generation(), move |_, args| {
            let controller = args.first().cloned().unwrap_or(Value::Undefined);
            match read_chunk(id) {
                Ok(Some(bytes)) => {
                    controller.call_method("enqueue", vec![uint8_array(&bytes)]);
                }
                Ok(None) => {
                    controller.call_method("close", vec![]);
                }
                Err(message) => {
                    controller.call_method("error", vec![error("NetworkError", &message)]);
                }
            }
            Value::Undefined
        }),
    );
    let readable =
        w3cos_core::class::construct(&crate::streams_web::readable_stream_class(), vec![source]);
    let sink = Value::object(HashMap::new());
    sink.set_property(
        "write",
        realm_function(crate::jsdom::realm_generation(), move |_, args| {
            let chunk = args.first().cloned().unwrap_or(Value::Undefined);
            match write_chunk(id, &bytes_of(&chunk)) {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(message) => rejected("NetworkError", &message),
            }
        }),
    );
    sink.set_property(
        "close",
        realm_function(crate::jsdom::realm_generation(), move |_, _| {
            let _ = close_socket(id);
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    let writable =
        w3cos_core::class::construct(&crate::streams_web::writable_stream_class(), vec![sink]);
    (readable, writable)
}

fn endpoint_value(address: &str, port: u16) -> (Value, Value) {
    (Value::string(address), Value::Number(f64::from(port)))
}

fn opened_info(id: u64, local: (&str, u16), remote: (&str, u16), include_remote: bool) -> Value {
    let (readable, writable) = stream_pair(id);
    let (local_address, local_port) = endpoint_value(local.0, local.1);
    let mut fields = HashMap::from([
        ("readable".into(), readable),
        ("writable".into(), writable),
        ("localAddress".into(), local_address),
        ("localPort".into(), local_port),
    ]);
    if include_remote {
        let (remote_address, remote_port) = endpoint_value(remote.0, remote.1);
        fields.insert("remoteAddress".into(), remote_address);
        fields.insert("remotePort".into(), remote_port);
    }
    Value::object(fields)
}

fn register(socket: LiveSocket) -> u64 {
    let id = next_id();
    SOCKETS.with(|sockets| {
        sockets.borrow_mut().insert(id, socket);
    });
    id
}

fn read_chunk(id: u64) -> Result<Option<Vec<u8>>, String> {
    SOCKETS.with(|sockets| {
        let mut sockets = sockets.borrow_mut();
        let Some(socket) = sockets.get_mut(&id) else {
            return Err("socket is closed".into());
        };
        match socket {
            LiveSocket::Tcp { reader, .. } => {
                let mut buffer = vec![0; 16 * 1024];
                match reader.read(&mut buffer) {
                    Ok(0) => Ok(None),
                    Ok(count) => {
                        buffer.truncate(count);
                        Ok(Some(buffer))
                    }
                    Err(err)
                        if err.kind() == std::io::ErrorKind::WouldBlock
                            || err.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        Err("timed out waiting for TCP data".into())
                    }
                    Err(err) => Err(err.to_string()),
                }
            }
            LiveSocket::Udp(socket) => {
                let mut buffer = vec![0; 64 * 1024];
                match socket.recv(&mut buffer) {
                    Ok(count) => {
                        buffer.truncate(count);
                        Ok(Some(buffer))
                    }
                    Err(err)
                        if err.kind() == std::io::ErrorKind::WouldBlock
                            || err.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        Err("timed out waiting for a UDP datagram".into())
                    }
                    Err(err) => Err(err.to_string()),
                }
            }
            LiveSocket::Listener(_) => Err("a TCP server socket has no byte stream".into()),
        }
    })
}

fn write_chunk(id: u64, bytes: &[u8]) -> Result<(), String> {
    SOCKETS.with(|sockets| {
        let mut sockets = sockets.borrow_mut();
        let Some(socket) = sockets.get_mut(&id) else {
            return Err("socket is closed".into());
        };
        match socket {
            LiveSocket::Tcp { writer, .. } => {
                writer.write_all(bytes).map_err(|err| err.to_string())
            }
            LiveSocket::Udp(socket) => socket
                .send(bytes)
                .map(|_| ())
                .map_err(|err| err.to_string()),
            LiveSocket::Listener(_) => Err("cannot write bytes to a TCP server socket".into()),
        }
    })
}

fn close_socket(id: u64) -> Result<(), String> {
    SOCKETS.with(|sockets| {
        sockets
            .borrow_mut()
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| "socket is closed".into())
    })
}

fn close_function(id: u64) -> Value {
    realm_function(
        crate::jsdom::realm_generation(),
        move |_, _| match close_socket(id) {
            Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
            Err(message) => rejected("InvalidStateError", &message),
        },
    )
}

fn adopt_tcp(stream: TcpStream) -> Result<Value, String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .map_err(|err| err.to_string())?;
    stream.set_nodelay(true).map_err(|err| err.to_string())?;
    let local = stream.local_addr().map_err(|err| err.to_string())?;
    let remote = stream.peer_addr().map_err(|err| err.to_string())?;
    let writer = stream.try_clone().map_err(|err| err.to_string())?;
    let id = register(LiveSocket::Tcp {
        reader: stream,
        writer,
    });
    let info = opened_info(
        id,
        (&local.ip().to_string(), local.port()),
        (&remote.ip().to_string(), remote.port()),
        true,
    );
    let socket = Value::object(HashMap::from([(
        "opened".into(),
        w3cos_core::promise::resolve(vec![info]),
    )]));
    socket.set_property("close", close_function(id));
    w3cos_core::class::set_prototype_of(&socket, &tcp_socket_class().get_property("prototype"));
    Ok(socket)
}

fn connect_tcp(address: &str, port: u16) -> Result<Value, String> {
    let mut candidates = (address, port)
        .to_socket_addrs()
        .map_err(|err| err.to_string())?;
    let Some(target) = candidates.next() else {
        return Err(format!("no address for {address}:{port}"));
    };
    let stream = TcpStream::connect_timeout(&target, Duration::from_secs(2))
        .map_err(|err| err.to_string())?;
    adopt_tcp(stream)
}

pub fn tcp_socket_class() -> Value {
    let generation = crate::jsdom::realm_generation();
    let constructor = realm_function(generation, move |this, args| {
        let address = args.first().map(Value::to_js_string).unwrap_or_default();
        let port = args.get(1).map(Value::to_u32).unwrap_or(0);
        if address.is_empty() || port == 0 || port > u16::MAX as u32 {
            w3cos_core::throw_value(error(
                "TypeError",
                "TCPSocket requires a remote address and port",
            ));
        }
        if let Some(rejected) = crate::capability_gate::reject_unless_granted(PERMISSION) {
            this.set_property("opened", rejected);
        } else {
            match connect_tcp(&address, port as u16) {
                Ok(adopted) => {
                    this.set_property("opened", adopted.get_property("opened"));
                    this.set_property("close", adopted.get_property("close"));
                }
                Err(message) => {
                    this.set_property("opened", rejected("NetworkError", &message));
                }
            }
        }
        if this.get_property("close").is_undefined() {
            this.set_property(
                "close",
                realm_function(generation, |_, _| {
                    rejected("InvalidStateError", "TCPSocket is not open")
                }),
            );
        }
        Value::Undefined
    });
    class_named("TCPSocket", constructor)
}

pub fn udp_socket_class() -> Value {
    let generation = crate::jsdom::realm_generation();
    let constructor = realm_function(generation, move |this, args| {
        let options = args.first().cloned().unwrap_or(Value::Undefined);
        if let Some(denied) = crate::capability_gate::reject_unless_granted(PERMISSION) {
            this.set_property("opened", denied);
            this.set_property(
                "close",
                realm_function(generation, |_, _| {
                    rejected("InvalidStateError", "UDPSocket is not open")
                }),
            );
        } else {
            match open_udp(&options) {
                Ok((id, info)) => {
                    this.set_property("opened", w3cos_core::promise::resolve(vec![info]));
                    this.set_property("close", close_function(id));
                }
                Err(message) => {
                    this.set_property("opened", rejected("NetworkError", &message));
                }
            }
        }
        Value::Undefined
    });
    class_named("UDPSocket", constructor)
}

fn open_udp(options: &Value) -> Result<(u64, Value), String> {
    let remote_address = options.get_property("remoteAddress").to_js_string();
    let remote_port = options.get_property("remotePort").to_u32();
    let local_address = options.get_property("localAddress").to_js_string();
    let local_port = options.get_property("localPort").to_u32();
    let bind_address = if local_address.is_empty() {
        format!("0.0.0.0:{}", local_port.min(u16::MAX as u32))
    } else {
        format!("{local_address}:{}", local_port.min(u16::MAX as u32))
    };
    let socket = UdpSocket::bind(&bind_address).map_err(|err| err.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_millis(800)))
        .map_err(|err| err.to_string())?;
    if !remote_address.is_empty() && remote_port > 0 && remote_port <= u16::MAX as u32 {
        socket
            .connect(format!("{remote_address}:{remote_port}"))
            .map_err(|err| err.to_string())?;
    }
    let local = socket.local_addr().map_err(|err| err.to_string())?;
    let id = register(LiveSocket::Udp(socket));
    Ok((
        id,
        opened_info(
            id,
            (&local.ip().to_string(), local.port()),
            (&remote_address, remote_port as u16),
            !remote_address.is_empty(),
        ),
    ))
}

pub fn tcp_server_socket_class() -> Value {
    let generation = crate::jsdom::realm_generation();
    let constructor = realm_function(generation, move |this, args| {
        let address = args
            .first()
            .map(Value::to_js_string)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "127.0.0.1".into());
        let options = args.get(1).cloned().unwrap_or(Value::Undefined);
        let port = options.get_property("localPort").to_u32();
        let opened =
            if let Some(rejected) = crate::capability_gate::reject_unless_granted(PERMISSION) {
                rejected
            } else {
                match open_server(&address, port) {
                    Ok(info) => w3cos_core::promise::resolve(vec![info]),
                    Err(message) => rejected("NetworkError", &message),
                }
            };
        this.set_property("opened", opened);
        this.set_property(
            "close",
            realm_function(generation, |_, _| {
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        Value::Undefined
    });
    class_named("TCPServerSocket", constructor)
}

fn open_server(address: &str, port: u32) -> Result<Value, String> {
    let bind = format!("{address}:{}", port.min(u16::MAX as u32));
    let listener = TcpListener::bind(&bind).map_err(|err| err.to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|err| err.to_string())?;
    let local = listener.local_addr().map_err(|err| err.to_string())?;
    let id = register(LiveSocket::Listener(listener));
    let source = Value::object(HashMap::new());
    source.set_property(
        "pull",
        realm_function(crate::jsdom::realm_generation(), move |_, args| {
            let controller = args.first().cloned().unwrap_or(Value::Undefined);
            match accept_one(id) {
                Ok(socket) => {
                    controller.call_method("enqueue", vec![socket]);
                }
                Err(message) => {
                    controller.call_method("error", vec![error("NetworkError", &message)]);
                }
            }
            Value::Undefined
        }),
    );
    let readable =
        w3cos_core::class::construct(&crate::streams_web::readable_stream_class(), vec![source]);
    Ok(Value::object(HashMap::from([
        ("readable".into(), readable),
        (
            "localAddress".into(),
            Value::string(&local.ip().to_string()),
        ),
        ("localPort".into(), Value::Number(f64::from(local.port()))),
    ])))
}

fn accept_one(id: u64) -> Result<Value, String> {
    let started = std::time::Instant::now();
    loop {
        let accepted = SOCKETS.with(|sockets| {
            let mut sockets = sockets.borrow_mut();
            let Some(LiveSocket::Listener(listener)) = sockets.get_mut(&id) else {
                return Err("TCP server socket is closed".to_string());
            };
            match listener.accept() {
                Ok((stream, _)) => Ok(Some(stream)),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
                Err(err) => Err(err.to_string()),
            }
        })?;
        if let Some(stream) = accepted {
            return adopt_tcp(stream);
        }
        if started.elapsed() > Duration::from_millis(800) {
            return Err("timed out waiting for a TCP connection".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn reset_realm() {
    SOCKETS.with(|sockets| sockets.borrow_mut().clear());
    CLASSES.with(|classes| classes.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::rc::Rc;

    fn promise_result(value: Value) -> Result<Value, String> {
        let ok = Rc::new(RefCell::new(None));
        let err = Rc::new(RefCell::new(None));
        let ok_for_then = Rc::clone(&ok);
        let err_for_catch = Rc::clone(&err);
        value.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *ok_for_then.borrow_mut() = args.first().cloned();
                Value::Undefined
            })],
        );
        value.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *err_for_catch.borrow_mut() = args
                    .first()
                    .map(|value| value.get_property("name").to_js_string());
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        if let Some(error) = err.borrow().clone() {
            return Err(error);
        }
        ok.borrow().clone().ok_or_else(|| "unresolved".into())
    }

    #[test]
    fn tcp_socket_requires_permission_then_exchanges_bytes() {
        reset_realm();
        crate::capability_gate::reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let denied = w3cos_core::class::construct(
            &tcp_socket_class(),
            vec![Value::string("127.0.0.1"), Value::Number(9.0)],
        );
        let error = promise_result(denied.get_property("opened")).unwrap_err();
        assert_eq!(error, "NotAllowedError");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 16];
            let count = stream.read(&mut buffer).unwrap();
            stream.write_all(&buffer[..count]).unwrap();
        });
        crate::capability_gate::grant(PERMISSION);
        let socket = w3cos_core::class::construct(
            &tcp_socket_class(),
            vec![Value::string("127.0.0.1"), Value::Number(f64::from(port))],
        );
        assert!(w3cos_core::class::instance_of(&socket, &tcp_socket_class()));
        let opened = promise_result(socket.get_property("opened")).unwrap();
        let writer = opened
            .get_property("writable")
            .call_method("getWriter", vec![]);
        promise_result(writer.call_method("write", vec![uint8_array(b"ping")])).unwrap();
        let reader = opened
            .get_property("readable")
            .call_method("getReader", vec![]);
        let chunk = promise_result(reader.call_method("read", vec![])).unwrap();
        let bytes = w3cos_core::binary::bytes_of(&chunk.get_property("value")).unwrap();
        assert_eq!(bytes, b"ping");
        server.join().unwrap();
        crate::capability_gate::reset();
        reset_realm();
    }
}
