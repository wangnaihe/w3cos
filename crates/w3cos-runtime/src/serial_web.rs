//! Web Serial (`navigator.serial`) on desktop hosts.
//!
//! `requestPort()` runs only after an explicit `serial` grant. With no browser
//! chooser, a host hook ([`set_serial_chooser`]) picks one enumerated port, or
//! a `usbVendorId` / `usbProductId` filter that matches exactly one port does.
//! Anything else rejects `NotFoundError`. Ports are never granted implicitly.
//!
//! macOS, Windows, and Linux desktop open the selected port with the
//! `serialport` crate. `readable` and `writable` move bytes on a dedicated I/O
//! thread. Mobile and OHOS have no adapter and keep the unavailable rejection.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, Once};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::jsdom::realm_function;
use w3cos_core::Value;

const PERMISSION: &str = "serial";
const HOTPLUG_INTERVAL: Duration = Duration::from_millis(400);
const IO_TIMEOUT: Duration = Duration::from_millis(20);
const MAX_BUFFER: u32 = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialPortRecord {
    pub name: String,
    pub usb_vendor_id: Option<u16>,
    pub usb_product_id: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParityMode {
    None,
    Even,
    Odd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlowMode {
    None,
    Hardware,
}

#[derive(Clone, Debug)]
struct OpenSettings {
    baud_rate: u32,
    data_bits: u8,
    stop_bits: u8,
    parity: ParityMode,
    buffer_size: usize,
    flow: FlowMode,
}

#[derive(Clone, Debug)]
struct OutputSignals {
    data_terminal_ready: Option<bool>,
    request_to_send: Option<bool>,
    break_signal: Option<bool>,
}

#[derive(Clone, Copy, Debug)]
struct InputSignals {
    data_carrier_detect: bool,
    clear_to_send: bool,
    ring_indicator: bool,
    data_set_ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Closed,
    Open,
}

struct Filter {
    usb_vendor_id: u16,
    usb_product_id: Option<u16>,
}

trait SerialIo: Send {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
    fn flush(&mut self) -> io::Result<()>;
    fn set_output_signals(&mut self, signals: &OutputSignals) -> Result<(), String>;
    fn input_signals(&mut self) -> Result<InputSignals, String>;
}

enum Command {
    Write(Vec<u8>, Sender<Result<(), String>>),
    SetSignals(OutputSignals, Sender<Result<(), String>>),
    GetSignals(Sender<Result<InputSignals, String>>),
    Close(Sender<()>),
}

struct Shared {
    incoming: Mutex<VecDeque<u8>>,
    fatal: Mutex<Option<String>>,
    stop: AtomicBool,
    buffer_size: usize,
}

struct OpenSession {
    shared: Arc<Shared>,
    commands: Sender<Command>,
    join: Option<JoinHandle<()>>,
}

struct PortEntry {
    record: SerialPortRecord,
    phase: Phase,
    connected: bool,
    announced: bool,
    object: Value,
    readable: Value,
    writable: Value,
    controller: Value,
    read_closed: bool,
    session: Option<OpenSession>,
    buffer_size: usize,
}

#[cfg(test)]
trait SerialBackend: Send + Sync {
    fn ports(&self) -> Result<Vec<SerialPortRecord>, String>;
    fn open(&self, name: &str, options: &OpenSettings) -> Result<Box<dyn SerialIo>, String>;
}

thread_local! {
    static SERIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PORT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MANAGER: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PORTS: RefCell<HashMap<u64, PortEntry>> = RefCell::new(HashMap::new());
    static ORDER: RefCell<Vec<u64>> = RefCell::new(Vec::new());
    static BY_NAME: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static CHOOSER: RefCell<Option<Arc<dyn Fn(&[SerialPortRecord]) -> Option<String>>>> =
        RefCell::new(None);
    static PORT_OVERRIDE: RefCell<Option<Vec<SerialPortRecord>>> = const { RefCell::new(None) };
    static ADAPTER_ENABLED: Cell<bool> = const { Cell::new(true) };
    static LAST_HOTPLUG: Cell<Option<Instant>> = const { Cell::new(None) };
    static POLLING: Cell<bool> = const { Cell::new(false) };
    static REFRESHING: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    static BACKEND: RefCell<Option<Arc<dyn SerialBackend>>> = RefCell::new(None);
}

static NO_ADAPTER_WARNING: Once = Once::new();

fn native_adapter_compiled() -> bool {
    cfg!(all(
        feature = "web-serial",
        any(
            target_os = "macos",
            target_os = "linux",
            target_os = "windows"
        ),
        not(target_env = "ohos")
    ))
}

fn dom_error(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn rejected(name: &str, message: &str) -> Value {
    w3cos_core::promise::reject(vec![dom_error(name, message)])
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
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

fn adapter_active() -> bool {
    if !ADAPTER_ENABLED.with(Cell::get) {
        return false;
    }
    #[cfg(test)]
    if BACKEND.with(|slot| slot.borrow().is_some()) {
        return true;
    }
    native_adapter_compiled()
}

fn ports_override() -> Option<Vec<SerialPortRecord>> {
    PORT_OVERRIDE.with(|slot| slot.borrow().clone())
}

fn list_ports() -> Result<Vec<SerialPortRecord>, String> {
    if let Some(ports) = ports_override() {
        return Ok(ports);
    }
    #[cfg(test)]
    if let Some(backend) = BACKEND.with(|slot| slot.borrow().clone()) {
        return backend.ports();
    }
    native_list()
}

fn open_io(name: &str, options: &OpenSettings) -> Result<Box<dyn SerialIo>, String> {
    #[cfg(test)]
    if let Some(backend) = BACKEND.with(|slot| slot.borrow().clone()) {
        return backend.open(name, options);
    }
    native_open(name, options)
}

#[cfg(all(
    feature = "web-serial",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn native_list() -> Result<Vec<SerialPortRecord>, String> {
    let ports = serialport::available_ports().map_err(|err| err.to_string())?;
    Ok(ports
        .into_iter()
        .map(|info| {
            let (usb_vendor_id, usb_product_id) = match info.port_type {
                serialport::SerialPortType::UsbPort(usb) => (Some(usb.vid), Some(usb.pid)),
                _ => (None, None),
            };
            SerialPortRecord {
                name: info.port_name,
                usb_vendor_id,
                usb_product_id,
            }
        })
        .collect())
}

#[cfg(not(all(
    feature = "web-serial",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn native_list() -> Result<Vec<SerialPortRecord>, String> {
    Err("Web Serial has no platform adapter".into())
}

#[cfg(all(
    feature = "web-serial",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn native_open(name: &str, options: &OpenSettings) -> Result<Box<dyn SerialIo>, String> {
    let data_bits = match options.data_bits {
        7 => serialport::DataBits::Seven,
        _ => serialport::DataBits::Eight,
    };
    let stop_bits = match options.stop_bits {
        2 => serialport::StopBits::Two,
        _ => serialport::StopBits::One,
    };
    let parity = match options.parity {
        ParityMode::Even => serialport::Parity::Even,
        ParityMode::Odd => serialport::Parity::Odd,
        ParityMode::None => serialport::Parity::None,
    };
    let flow = match options.flow {
        FlowMode::Hardware => serialport::FlowControl::Hardware,
        FlowMode::None => serialport::FlowControl::None,
    };
    let port = serialport::new(name, options.baud_rate)
        .data_bits(data_bits)
        .stop_bits(stop_bits)
        .parity(parity)
        .flow_control(flow)
        .timeout(IO_TIMEOUT)
        .open()
        .map_err(|err| err.to_string())?;
    Ok(Box::new(NativePort { port }))
}

#[cfg(not(all(
    feature = "web-serial",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn native_open(_name: &str, _options: &OpenSettings) -> Result<Box<dyn SerialIo>, String> {
    Err("Web Serial has no platform adapter".into())
}

#[cfg(all(
    feature = "web-serial",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
struct NativePort {
    port: Box<dyn serialport::SerialPort>,
}

#[cfg(all(
    feature = "web-serial",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
impl SerialIo for NativePort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(&mut *self.port, buf)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Write::write(&mut *self.port, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut *self.port)
    }

    fn set_output_signals(&mut self, signals: &OutputSignals) -> Result<(), String> {
        if let Some(level) = signals.data_terminal_ready {
            self.port
                .write_data_terminal_ready(level)
                .map_err(|err| err.to_string())?;
        }
        if let Some(level) = signals.request_to_send {
            self.port
                .write_request_to_send(level)
                .map_err(|err| err.to_string())?;
        }
        if let Some(level) = signals.break_signal {
            if level {
                self.port.set_break().map_err(|err| err.to_string())?;
            } else {
                self.port.clear_break().map_err(|err| err.to_string())?;
            }
        }
        Ok(())
    }

    fn input_signals(&mut self) -> Result<InputSignals, String> {
        Ok(InputSignals {
            data_carrier_detect: self
                .port
                .read_carrier_detect()
                .map_err(|err| err.to_string())?,
            clear_to_send: self
                .port
                .read_clear_to_send()
                .map_err(|err| err.to_string())?,
            ring_indicator: self
                .port
                .read_ring_indicator()
                .map_err(|err| err.to_string())?,
            data_set_ready: self
                .port
                .read_data_set_ready()
                .map_err(|err| err.to_string())?,
        })
    }
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn write_all(io: &mut dyn SerialIo, mut bytes: &[u8]) -> Result<(), String> {
    while !bytes.is_empty() {
        match io.write(bytes) {
            Ok(0) => return Err("serial port accepted no bytes".into()),
            Ok(count) => bytes = &bytes[count..],
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.to_string()),
        }
    }
    io.flush().map_err(|err| err.to_string())
}

fn handle_command(io: &mut dyn SerialIo, command: Command, shared: &Shared) -> bool {
    match command {
        Command::Write(bytes, reply) => {
            let _ = reply.send(write_all(io, &bytes));
            true
        }
        Command::SetSignals(signals, reply) => {
            let _ = reply.send(io.set_output_signals(&signals));
            true
        }
        Command::GetSignals(reply) => {
            let _ = reply.send(io.input_signals());
            true
        }
        Command::Close(reply) => {
            shared.stop.store(true, Ordering::Relaxed);
            let _ = reply.send(());
            false
        }
    }
}

fn worker(mut io: Box<dyn SerialIo>, commands: Receiver<Command>, shared: Arc<Shared>) {
    let mut buffer = vec![0u8; shared.buffer_size.max(1)];
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if !handle_command(&mut *io, command, &shared) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        match io.read(&mut buffer) {
            Ok(0) => thread::sleep(Duration::from_millis(4)),
            Ok(count) => lock(&shared.incoming).extend(buffer[..count].iter().copied()),
            Err(err) if is_timeout(&err) => thread::sleep(Duration::from_millis(4)),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                *lock(&shared.fatal) = Some(err.to_string());
                break;
            }
        }
    }
}

fn spawn_session(io: Box<dyn SerialIo>, buffer_size: usize) -> Result<OpenSession, String> {
    let shared = Arc::new(Shared {
        incoming: Mutex::new(VecDeque::new()),
        fatal: Mutex::new(None),
        stop: AtomicBool::new(false),
        buffer_size: buffer_size.max(1),
    });
    let (commands, incoming) = mpsc::channel();
    let shared_for_thread = Arc::clone(&shared);
    let join = thread::Builder::new()
        .name("w3cos-serial".into())
        .spawn(move || worker(io, incoming, shared_for_thread))
        .map_err(|err| err.to_string())?;
    Ok(OpenSession {
        shared,
        commands,
        join: Some(join),
    })
}

fn shutdown(mut session: OpenSession) {
    session.shared.stop.store(true, Ordering::Relaxed);
    let (reply, wait) = mpsc::channel();
    let _ = session.commands.send(Command::Close(reply));
    let _ = wait.recv_timeout(Duration::from_millis(500));
    if let Some(handle) = session.join.take() {
        let _ = handle.join();
    }
}

fn session_call<T>(
    id: u64,
    build: impl FnOnce(Sender<Result<T, String>>) -> Command,
) -> Result<T, String> {
    let commands = PORTS.with(|ports| {
        ports
            .borrow()
            .get(&id)
            .and_then(|entry| entry.session.as_ref())
            .map(|session| session.commands.clone())
    });
    let Some(commands) = commands else {
        return Err("serial port is closed".into());
    };
    let (reply, wait) = mpsc::channel();
    commands
        .send(build(reply))
        .map_err(|_| "serial port is closed".to_string())?;
    wait.recv_timeout(Duration::from_secs(3))
        .map_err(|_| "serial port timed out".to_string())?
}

fn illegal_constructor(name: &'static str) -> Value {
    let generation = crate::jsdom::realm_generation();
    let class = realm_function(generation, move |_, _| {
        w3cos_core::throw_value(dom_error(
            "TypeError",
            &format!("Illegal constructor: {name}"),
        ))
    });
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    w3cos_core::class::set_prototype_of(
        &prototype,
        &crate::web_events::event_target_class().get_property("prototype"),
    );
    class.set_property("prototype", prototype);
    class
}

fn bind(prototype: &Value, name: &str, body: impl Fn(Value, Vec<Value>) -> Value + 'static) {
    let generation = crate::jsdom::realm_generation();
    prototype.set_property(name, realm_function(generation, body));
}

pub fn serial_class() -> Value {
    SERIAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_constructor("Serial");
        let prototype = class.get_property("prototype");
        bind(&prototype, "getPorts", |_, _| get_ports());
        bind(&prototype, "requestPort", |_, args| {
            request_port(args.first().cloned().unwrap_or(Value::Undefined))
        });
        for property in ["onconnect", "ondisconnect"] {
            prototype.set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn serial_port_class() -> Value {
    PORT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_constructor("SerialPort");
        let prototype = class.get_property("prototype");
        bind(&prototype, "open", |this, args| {
            with_port(&this, |id| {
                open_port(id, &args.first().cloned().unwrap_or(Value::Undefined))
            })
        });
        bind(&prototype, "close", |this, _| {
            with_port(&this, |id| close_port(id))
        });
        bind(&prototype, "forget", |this, _| {
            with_port(&this, |id| forget_port(id))
        });
        bind(&prototype, "getInfo", |this, _| {
            let Some(id) = id_of(&this) else {
                return Value::object(HashMap::new());
            };
            info_value(id)
        });
        bind(&prototype, "getSignals", |this, _| {
            with_port(&this, |id| get_signals(id))
        });
        bind(&prototype, "setSignals", |this, args| {
            with_port(&this, |id| {
                set_signals(id, &args.first().cloned().unwrap_or(Value::Undefined))
            })
        });
        for property in [
            "connected",
            "onconnect",
            "ondisconnect",
            "readable",
            "writable",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn id_of(value: &Value) -> Option<u64> {
    let id = value.get_property("__w3cos_serial_id");
    if id.is_number() {
        Some(id.to_number() as u64)
    } else {
        None
    }
}

fn with_port(this: &Value, body: impl FnOnce(u64) -> Value) -> Value {
    match id_of(this) {
        Some(id) => body(id),
        None => rejected("InvalidStateError", "serial port is not available"),
    }
}

fn port_field(id: u64, read: impl FnOnce(&PortEntry) -> Value) -> Value {
    PORTS.with(|ports| {
        ports
            .borrow()
            .get(&id)
            .map(read)
            .unwrap_or(Value::Undefined)
    })
}

fn make_port_object(id: u64) -> Value {
    let generation = crate::jsdom::realm_generation();
    let port = Value::object(HashMap::new());
    crate::web_events::event_target_class().call(port.clone(), vec![]);
    w3cos_core::class::set_prototype_of(&port, &serial_port_class().get_property("prototype"));
    port.set_property("__w3cos_serial_id", Value::Number(id as f64));
    port.set_property("onconnect", Value::Null);
    port.set_property("ondisconnect", Value::Null);
    port.set_property(
        "__w3cos_getter_connected",
        realm_function(generation, move |_, _| {
            Value::Bool(port_field(id, |entry| Value::Bool(entry.connected)).to_bool())
        }),
    );
    port.set_property(
        "__w3cos_getter_readable",
        realm_function(generation, move |_, _| {
            port_field(id, |entry| entry.readable.clone())
        }),
    );
    port.set_property(
        "__w3cos_getter_writable",
        realm_function(generation, move |_, _| {
            port_field(id, |entry| entry.writable.clone())
        }),
    );
    port
}

pub fn serial_value() -> Value {
    if let Some(existing) = MANAGER.with(|slot| slot.borrow().clone()) {
        return existing;
    }
    let manager = Value::object(HashMap::new());
    crate::web_events::event_target_class().call(manager.clone(), vec![]);
    w3cos_core::class::set_prototype_of(&manager, &serial_class().get_property("prototype"));
    manager.set_property("onconnect", Value::Null);
    manager.set_property("ondisconnect", Value::Null);
    MANAGER.with(|slot| *slot.borrow_mut() = Some(manager.clone()));
    manager
}

/// Host picker used when the page calls `requestPort()`.
///
/// The hook receives the ports that passed `filters` (or every enumerated
/// port when filters were omitted) and returns the chosen port name. `None`
/// cancels the request with `NotFoundError`. A name outside that list is
/// rejected the same way.
pub fn set_serial_chooser(chooser: Option<Arc<dyn Fn(&[SerialPortRecord]) -> Option<String>>>) {
    CHOOSER.with(|slot| *slot.borrow_mut() = chooser);
}

/// Replaces platform enumeration. `None` restores the host list.
///
/// This does not grant a port. `requestPort()` still requires the `serial`
/// permission and a chooser selection or a single filter match.
pub fn set_serial_ports_override(ports: Option<Vec<SerialPortRecord>>) {
    PORT_OVERRIDE.with(|slot| *slot.borrow_mut() = ports);
}

#[cfg(test)]
fn set_backend(backend: Option<Arc<dyn SerialBackend>>) {
    BACKEND.with(|slot| *slot.borrow_mut() = backend);
}

#[cfg(test)]
fn set_adapter_enabled(enabled: bool) {
    ADAPTER_ENABLED.with(|slot| slot.set(enabled));
}

fn next_id() -> u64 {
    NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id.saturating_add(1));
        id
    })
}

fn grant_port(record: SerialPortRecord) -> Value {
    if let Some(id) = BY_NAME.with(|names| names.borrow().get(&record.name).copied()) {
        PORTS.with(|ports| {
            if let Some(entry) = ports.borrow_mut().get_mut(&id) {
                entry.record = record;
                entry.connected = true;
                entry.announced = true;
            }
        });
        return port_field(id, |entry| entry.object.clone());
    }
    let id = next_id();
    let object = make_port_object(id);
    BY_NAME.with(|names| {
        names.borrow_mut().insert(record.name.clone(), id);
    });
    ORDER.with(|order| order.borrow_mut().push(id));
    PORTS.with(|ports| {
        ports.borrow_mut().insert(
            id,
            PortEntry {
                record,
                phase: Phase::Closed,
                connected: true,
                announced: true,
                object: object.clone(),
                readable: Value::Null,
                writable: Value::Null,
                controller: Value::Undefined,
                read_closed: false,
                session: None,
                buffer_size: 255,
            },
        );
    });
    object
}

fn parse_u32(value: &Value, name: &str) -> Result<u32, Value> {
    if !value.is_number() {
        return Err(dom_error(
            "TypeError",
            &format!("{name} must be an integer"),
        ));
    }
    let number = value.to_number();
    if !number.is_finite() || number <= 0.0 || number.fract() != 0.0 || number > f64::from(u32::MAX)
    {
        return Err(dom_error(
            "TypeError",
            &format!("{name} must be a positive integer"),
        ));
    }
    Ok(number as u32)
}

fn parse_u16(value: &Value, name: &str) -> Result<u16, Value> {
    if !value.is_number() {
        return Err(dom_error(
            "TypeError",
            &format!("{name} must be an integer"),
        ));
    }
    let number = value.to_number();
    if !number.is_finite() || number < 0.0 || number > f64::from(u16::MAX) || number.fract() != 0.0
    {
        return Err(dom_error(
            "TypeError",
            &format!("{name} must be an integer from 0 to 65535"),
        ));
    }
    Ok(number as u16)
}

fn optional_u32(options: &Value, name: &str, default: u32) -> Result<u32, Value> {
    let value = options.get_property(name);
    if value.is_undefined() {
        Ok(default)
    } else {
        parse_u32(&value, name)
    }
}

fn parse_filters(options: &Value) -> Result<Vec<Filter>, Value> {
    if options.is_undefined() || options.is_null() {
        return Ok(Vec::new());
    }
    if !options.is_object() {
        return Err(dom_error(
            "TypeError",
            "SerialPortRequestOptions must be an object",
        ));
    }
    let filters = options.get_property("filters");
    if filters.is_undefined() {
        return Ok(Vec::new());
    }
    if !filters.is_array() {
        return Err(dom_error("TypeError", "filters must be a sequence"));
    }
    let length = filters.get_property("length").to_number();
    if !length.is_finite() || length < 0.0 {
        return Err(dom_error("TypeError", "filters must be a sequence"));
    }
    let mut parsed = Vec::new();
    for index in 0..length as usize {
        let filter = filters.get_property(&index.to_string());
        if !filter.is_object() {
            return Err(dom_error("TypeError", "SerialPortFilter must be an object"));
        }
        if !filter
            .get_property("bluetoothServiceClassId")
            .is_undefined()
        {
            return Err(dom_error(
                "TypeError",
                "Web Serial bluetooth filters are not supported",
            ));
        }
        let vendor = filter.get_property("usbVendorId");
        let product = filter.get_property("usbProductId");
        if vendor.is_undefined() {
            return Err(dom_error(
                "TypeError",
                "SerialPortFilter requires usbVendorId",
            ));
        }
        parsed.push(Filter {
            usb_vendor_id: parse_u16(&vendor, "usbVendorId")?,
            usb_product_id: if product.is_undefined() {
                None
            } else {
                Some(parse_u16(&product, "usbProductId")?)
            },
        });
    }
    Ok(parsed)
}

fn matches_filter(port: &SerialPortRecord, filter: &Filter) -> bool {
    if port.usb_vendor_id != Some(filter.usb_vendor_id) {
        return false;
    }
    match filter.usb_product_id {
        Some(product) => port.usb_product_id == Some(product),
        None => true,
    }
}

enum Choice {
    Absent,
    Cancelled,
    Picked(String),
}

fn invoke_chooser(candidates: &[SerialPortRecord]) -> Choice {
    let chooser = CHOOSER.with(|slot| slot.borrow().clone());
    let Some(chooser) = chooser else {
        return Choice::Absent;
    };
    match chooser(candidates) {
        Some(name) => Choice::Picked(name),
        None => Choice::Cancelled,
    }
}

fn select_port(filters: &[Filter]) -> Result<SerialPortRecord, Value> {
    let listed = list_ports().map_err(|message| dom_error("NetworkError", &message))?;
    let candidates = if filters.is_empty() {
        listed
    } else {
        listed
            .into_iter()
            .filter(|port| filters.iter().any(|filter| matches_filter(port, filter)))
            .collect::<Vec<_>>()
    };
    match invoke_chooser(&candidates) {
        Choice::Cancelled => Err(dom_error(
            "NotFoundError",
            "requestPort did not select a serial port",
        )),
        Choice::Picked(name) => candidates
            .into_iter()
            .find(|port| port.name == name)
            .ok_or_else(|| {
                dom_error(
                    "NotFoundError",
                    "requestPort chooser returned a port that is not available",
                )
            }),
        Choice::Absent if filters.is_empty() => Err(dom_error(
            "NotFoundError",
            "requestPort did not select a serial port; no chooser is configured",
        )),
        Choice::Absent => match candidates.as_slice() {
            [only] => Ok(only.clone()),
            [] => Err(dom_error(
                "NotFoundError",
                "requestPort could not select a serial port because no port matched the filters",
            )),
            _ => Err(dom_error(
                "NotFoundError",
                "requestPort matched more than one serial port; a chooser is required",
            )),
        },
    }
}

fn request_port(options: Value) -> Value {
    if !adapter_active() {
        NO_ADAPTER_WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: Web Serial exposes a compatibility surface; native discovery, \
                 permission prompts and device I/O require a platform adapter"
            );
        });
        return rejected(
            "NotFoundError",
            "requestPort could not select a device because no Web Serial adapter is configured",
        );
    }
    if let Some(rejected) = crate::capability_gate::reject_unless_granted(PERMISSION) {
        return rejected;
    }
    let filters = match parse_filters(&options) {
        Ok(filters) => filters,
        Err(error) => return w3cos_core::promise::reject(vec![error]),
    };
    match select_port(&filters) {
        Ok(record) => w3cos_core::promise::resolve(vec![grant_port(record)]),
        Err(error) => w3cos_core::promise::reject(vec![error]),
    }
}

fn get_ports() -> Value {
    if adapter_active() {
        let _ = refresh_connections(true);
    }
    let objects = ORDER.with(|order| {
        let order = order.borrow();
        PORTS.with(|ports| {
            let ports = ports.borrow();
            order
                .iter()
                .filter_map(|id| ports.get(id).map(|entry| entry.object.clone()))
                .collect::<Vec<_>>()
        })
    });
    w3cos_core::promise::resolve(vec![Value::array(objects)])
}

fn info_value(id: u64) -> Value {
    let (vendor, product) = PORTS
        .with(|ports| {
            ports
                .borrow()
                .get(&id)
                .map(|entry| (entry.record.usb_vendor_id, entry.record.usb_product_id))
        })
        .unwrap_or((None, None));
    let info = Value::object(HashMap::new());
    if let Some(vendor) = vendor {
        info.set_property("usbVendorId", Value::Number(f64::from(vendor)));
    }
    if let Some(product) = product {
        info.set_property("usbProductId", Value::Number(f64::from(product)));
    }
    info
}

fn parse_open_options(options: &Value) -> Result<OpenSettings, Value> {
    if !options.is_object() {
        return Err(dom_error("TypeError", "SerialOptions.baudRate is required"));
    }
    let baud = options.get_property("baudRate");
    if baud.is_undefined() {
        return Err(dom_error("TypeError", "SerialOptions.baudRate is required"));
    }
    let baud_rate = parse_u32(&baud, "baudRate")?;
    let data_bits = optional_u32(options, "dataBits", 8)?;
    if data_bits != 7 && data_bits != 8 {
        return Err(dom_error("TypeError", "dataBits must be 7 or 8"));
    }
    let stop_bits = optional_u32(options, "stopBits", 1)?;
    if stop_bits != 1 && stop_bits != 2 {
        return Err(dom_error("TypeError", "stopBits must be 1 or 2"));
    }
    let parity_value = options.get_property("parity");
    let parity = if parity_value.is_undefined() {
        ParityMode::None
    } else {
        match parity_value.to_js_string().as_str() {
            "none" => ParityMode::None,
            "even" => ParityMode::Even,
            "odd" => ParityMode::Odd,
            _ => {
                return Err(dom_error(
                    "TypeError",
                    "parity must be \"none\", \"even\", or \"odd\"",
                ));
            }
        }
    };
    let buffer_size = optional_u32(options, "bufferSize", 255)?;
    if buffer_size > MAX_BUFFER {
        return Err(dom_error("TypeError", "bufferSize is too large"));
    }
    let flow_value = options.get_property("flowControl");
    let flow = if flow_value.is_undefined() {
        FlowMode::None
    } else {
        match flow_value.to_js_string().as_str() {
            "none" => FlowMode::None,
            "hardware" => FlowMode::Hardware,
            _ => {
                return Err(dom_error(
                    "TypeError",
                    "flowControl must be \"none\" or \"hardware\"",
                ));
            }
        }
    };
    Ok(OpenSettings {
        baud_rate,
        data_bits: data_bits as u8,
        stop_bits: stop_bits as u8,
        parity,
        buffer_size: buffer_size as usize,
        flow,
    })
}

fn open_port(id: u64, options: &Value) -> Value {
    let settings = match parse_open_options(options) {
        Ok(settings) => settings,
        Err(error) => return w3cos_core::promise::reject(vec![error]),
    };
    let name = PORTS.with(|ports| {
        let ports = ports.borrow();
        let Some(entry) = ports.get(&id) else {
            return Err(dom_error(
                "InvalidStateError",
                "serial port is not available",
            ));
        };
        if entry.phase == Phase::Open {
            return Err(dom_error(
                "InvalidStateError",
                "serial port is already open",
            ));
        }
        Ok(entry.record.name.clone())
    });
    let name = match name {
        Ok(name) => name,
        Err(error) => return w3cos_core::promise::reject(vec![error]),
    };
    let io = match open_io(&name, &settings) {
        Ok(io) => io,
        Err(message) => return rejected("NetworkError", &message),
    };
    let session = match spawn_session(io, settings.buffer_size) {
        Ok(session) => session,
        Err(message) => return rejected("NetworkError", &message),
    };
    let generation = crate::jsdom::realm_generation();
    let source = Value::object(HashMap::from([("type".into(), Value::string("bytes"))]));
    source.set_property(
        "pull",
        realm_function(generation, move |_, args| {
            let controller = args.first().cloned().unwrap_or(Value::Undefined);
            pull_serial(id, &controller);
            Value::Undefined
        }),
    );
    let readable =
        w3cos_core::class::construct(&crate::streams_web::readable_stream_class(), vec![source]);
    let sink = Value::object(HashMap::new());
    sink.set_property(
        "write",
        realm_function(generation, move |_, args| {
            let chunk = args.first().cloned().unwrap_or(Value::Undefined);
            match session_call(id, |reply| Command::Write(bytes_of(&chunk), reply)) {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(message) => rejected("NetworkError", &message),
            }
        }),
    );
    sink.set_property(
        "close",
        realm_function(generation, |_, _| {
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    let writable =
        w3cos_core::class::construct(&crate::streams_web::writable_stream_class(), vec![sink]);
    let mut leftover = Some(session);
    let _stored = PORTS.with(|ports| {
        let mut ports = ports.borrow_mut();
        let Some(entry) = ports.get_mut(&id) else {
            return false;
        };
        entry.phase = Phase::Open;
        entry.buffer_size = settings.buffer_size;
        entry.read_closed = false;
        entry.controller = Value::Undefined;
        entry.readable = readable;
        entry.writable = writable;
        entry.session = leftover.take();
        true
    });
    if let Some(session) = leftover {
        shutdown(session);
        return rejected("InvalidStateError", "serial port is not available");
    }
    w3cos_core::promise::resolve(vec![Value::Undefined])
}

fn finish_close(id: u64, session: Option<OpenSession>) -> Value {
    if let Some(session) = session {
        shutdown(session);
    }
    let controller = PORTS.with(|ports| {
        let mut ports = ports.borrow_mut();
        let Some(entry) = ports.get_mut(&id) else {
            return Value::Undefined;
        };
        entry.phase = Phase::Closed;
        entry.readable = Value::Null;
        entry.writable = Value::Null;
        entry.session = None;
        let controller = std::mem::replace(&mut entry.controller, Value::Undefined);
        if entry.read_closed {
            Value::Undefined
        } else {
            entry.read_closed = true;
            controller
        }
    });
    if !controller.is_undefined() {
        controller.call_method("close", vec![]);
    }
    w3cos_core::promise::resolve(vec![Value::Undefined])
}

fn close_port(id: u64) -> Value {
    let taken = PORTS.with(|ports| {
        let mut ports = ports.borrow_mut();
        let Some(entry) = ports.get_mut(&id) else {
            return Err(dom_error(
                "InvalidStateError",
                "serial port is not available",
            ));
        };
        if entry.phase != Phase::Open {
            return Err(dom_error("InvalidStateError", "serial port is not open"));
        }
        entry.phase = Phase::Closed;
        Ok(entry.session.take())
    });
    match taken {
        Ok(session) => finish_close(id, session),
        Err(error) => w3cos_core::promise::reject(vec![error]),
    }
}

fn forget_port(id: u64) -> Value {
    let open = PORTS.with(|ports| {
        ports
            .borrow()
            .get(&id)
            .is_some_and(|entry| entry.phase == Phase::Open)
    });
    if open {
        let _ = close_port(id);
    }
    let name = PORTS.with(|ports| {
        ports
            .borrow_mut()
            .remove(&id)
            .map(|entry| entry.record.name)
    });
    if let Some(name) = name {
        BY_NAME.with(|names| {
            names.borrow_mut().remove(&name);
        });
        ORDER.with(|order| order.borrow_mut().retain(|stored| *stored != id));
    }
    w3cos_core::promise::resolve(vec![Value::Undefined])
}

fn optional_bool(value: &Value, name: &str) -> Result<Option<bool>, Value> {
    if value.is_undefined() {
        Ok(None)
    } else if value.is_bool() {
        Ok(Some(value.to_bool()))
    } else {
        Err(dom_error("TypeError", &format!("{name} must be a boolean")))
    }
}

fn set_signals(id: u64, options: &Value) -> Value {
    if !options.is_undefined() && !options.is_object() {
        return rejected("TypeError", "SerialOutputSignals must be an object");
    }
    let signals = if options.is_undefined() {
        OutputSignals {
            data_terminal_ready: None,
            request_to_send: None,
            break_signal: None,
        }
    } else {
        match (
            optional_bool(
                &options.get_property("dataTerminalReady"),
                "dataTerminalReady",
            ),
            optional_bool(&options.get_property("requestToSend"), "requestToSend"),
            optional_bool(&options.get_property("break"), "break"),
        ) {
            (Ok(data_terminal_ready), Ok(request_to_send), Ok(break_signal)) => OutputSignals {
                data_terminal_ready,
                request_to_send,
                break_signal,
            },
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                return w3cos_core::promise::reject(vec![error]);
            }
        }
    };
    let open = PORTS.with(|ports| {
        ports
            .borrow()
            .get(&id)
            .is_some_and(|entry| entry.phase == Phase::Open)
    });
    if !open {
        return rejected("InvalidStateError", "serial port is not open");
    }
    match session_call(id, |reply| Command::SetSignals(signals, reply)) {
        Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
        Err(message) => rejected("NetworkError", &message),
    }
}

fn signals_value(signals: InputSignals) -> Value {
    Value::object(HashMap::from([
        (
            "dataCarrierDetect".into(),
            Value::Bool(signals.data_carrier_detect),
        ),
        ("clearToSend".into(), Value::Bool(signals.clear_to_send)),
        ("ringIndicator".into(), Value::Bool(signals.ring_indicator)),
        ("dataSetReady".into(), Value::Bool(signals.data_set_ready)),
    ]))
}

fn get_signals(id: u64) -> Value {
    let open = PORTS.with(|ports| {
        ports
            .borrow()
            .get(&id)
            .is_some_and(|entry| entry.phase == Phase::Open)
    });
    if !open {
        return rejected("InvalidStateError", "serial port is not open");
    }
    match session_call(id, Command::GetSignals) {
        Ok(signals) => w3cos_core::promise::resolve(vec![signals_value(signals)]),
        Err(message) => rejected("NetworkError", &message),
    }
}

fn pull_serial(id: u64, controller: &Value) {
    let snapshot = PORTS.with(|ports| {
        let mut ports = ports.borrow_mut();
        let Some(entry) = ports.get_mut(&id) else {
            return Some((Vec::new(), Some("serial port is closed".into()), true, true));
        };
        if entry.read_closed {
            return None;
        }
        entry.controller = controller.clone();
        let Some(session) = &entry.session else {
            return Some((Vec::new(), None, true, false));
        };
        let fatal = lock(&session.shared.fatal).clone();
        let mut incoming = lock(&session.shared.incoming);
        let count = incoming.len().min(entry.buffer_size.max(1));
        let bytes = incoming.drain(..count).collect::<Vec<_>>();
        let stopped = session.shared.stop.load(Ordering::Relaxed);
        Some((bytes, fatal, stopped, false))
    });
    let Some((bytes, fatal, stopped, missing)) = snapshot else {
        return;
    };
    if missing {
        return;
    }
    if let Some(message) = fatal {
        mark_read_closed(id);
        controller.call_method("error", vec![dom_error("NetworkError", &message)]);
        return;
    }
    if !bytes.is_empty() {
        controller.call_method("enqueue", vec![uint8_array(&bytes)]);
        return;
    }
    if stopped {
        mark_read_closed(id);
        controller.call_method("close", vec![]);
    }
}

fn mark_read_closed(id: u64) {
    PORTS.with(|ports| {
        if let Some(entry) = ports.borrow_mut().get_mut(&id) {
            entry.read_closed = true;
        }
    });
}

fn dispatch_connection(target: &Value, kind: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![
            Value::string(kind),
            Value::object(HashMap::from([("bubbles".into(), Value::Bool(true))])),
        ],
    );
    target.call_method("dispatchEvent", vec![event]);
}

fn refresh_connections(force: bool) -> usize {
    if REFRESHING.with(Cell::get) {
        return 0;
    }
    let granted = ORDER.with(|order| !order.borrow().is_empty());
    if !granted || !adapter_active() {
        return 0;
    }
    if !force {
        let due = LAST_HOTPLUG.with(|last| {
            last.get()
                .is_none_or(|instant| instant.elapsed() >= HOTPLUG_INTERVAL)
        });
        if !due {
            return 0;
        }
    }
    let listed = match list_ports() {
        Ok(ports) => ports,
        Err(_) => return 0,
    };
    LAST_HOTPLUG.with(|last| last.set(Some(Instant::now())));
    REFRESHING.with(|flag| flag.set(true));
    struct RefreshGuard;
    impl Drop for RefreshGuard {
        fn drop(&mut self) {
            REFRESHING.with(|flag| flag.set(false));
        }
    }
    let _refresh_guard = RefreshGuard;
    let names = listed
        .iter()
        .map(|port| port.name.clone())
        .collect::<HashSet<_>>();
    let mut events = Vec::new();
    PORTS.with(|ports| {
        for entry in ports.borrow_mut().values_mut() {
            let present = names.contains(&entry.record.name);
            if present && !entry.announced {
                entry.connected = true;
                entry.announced = true;
                events.push((entry.object.clone(), "connect"));
            } else if !present && entry.announced {
                entry.connected = false;
                entry.announced = false;
                events.push((entry.object.clone(), "disconnect"));
            } else if present {
                entry.connected = true;
            }
        }
    });
    let manager = MANAGER.with(|slot| slot.borrow().clone());
    for (port, kind) in &events {
        dispatch_connection(port, kind);
        if let Some(manager) = &manager {
            dispatch_connection(manager, kind);
        }
    }
    events.len()
}

fn drain_readable() -> usize {
    let mut jobs = Vec::new();
    PORTS.with(|ports| {
        for (id, entry) in ports.borrow().iter() {
            if entry.phase != Phase::Open || entry.read_closed || entry.controller.is_undefined() {
                continue;
            }
            let Some(session) = &entry.session else {
                continue;
            };
            let fatal = lock(&session.shared.fatal).clone();
            let available = lock(&session.shared.incoming).len();
            let stopped = session.shared.stop.load(Ordering::Relaxed);
            if fatal.is_none() && available == 0 && !stopped {
                continue;
            }
            jobs.push((*id, entry.controller.clone(), fatal, stopped));
        }
    });
    let mut delivered = 0;
    for (id, controller, fatal, stopped) in jobs {
        let bytes = PORTS.with(|ports| {
            let ports = ports.borrow();
            let Some(entry) = ports.get(&id) else {
                return Vec::new();
            };
            let limit = entry.buffer_size.max(1);
            let Some(session) = entry.session.as_ref() else {
                return Vec::new();
            };
            let mut incoming = lock(&session.shared.incoming);
            let count = incoming.len().min(limit);
            incoming.drain(..count).collect::<Vec<_>>()
        });
        if let Some(message) = fatal {
            mark_read_closed(id);
            controller.call_method("error", vec![dom_error("NetworkError", &message)]);
            delivered += 1;
            continue;
        }
        if !bytes.is_empty() {
            controller.call_method("enqueue", vec![uint8_array(&bytes)]);
            delivered += 1;
            continue;
        }
        if stopped {
            mark_read_closed(id);
            controller.call_method("close", vec![]);
            delivered += 1;
        }
    }
    delivered
}

/// Moves bytes from open serial ports into their readable streams and polls
/// for connect and disconnect. The frame loop calls this from
/// `drain_microtasks`.
pub fn poll_serial_io() -> usize {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            POLLING.with(|flag| flag.set(false));
        }
    }
    if POLLING.with(Cell::get) {
        return 0;
    }
    POLLING.with(|flag| flag.set(true));
    let _guard = Guard;
    refresh_connections(false) + drain_readable()
}

pub fn reset_realm() {
    let sessions = PORTS.with(|ports| {
        ports
            .borrow_mut()
            .drain()
            .filter_map(|(_, entry)| entry.session)
            .collect::<Vec<_>>()
    });
    for session in sessions {
        shutdown(session);
    }
    ORDER.with(|order| order.borrow_mut().clear());
    BY_NAME.with(|names| names.borrow_mut().clear());
    NEXT_ID.with(|next| next.set(1));
    CHOOSER.with(|slot| *slot.borrow_mut() = None);
    PORT_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    ADAPTER_ENABLED.with(|slot| slot.set(true));
    LAST_HOTPLUG.with(|slot| slot.set(None));
    MANAGER.with(|slot| *slot.borrow_mut() = None);
    SERIAL_CLASS.with(|slot| *slot.borrow_mut() = None);
    PORT_CLASS.with(|slot| *slot.borrow_mut() = None);
    #[cfg(test)]
    BACKEND.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Instant;

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            reset_realm();
        }
    }

    fn setup() -> Guard {
        let _ = Guard;
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::capability_gate::reset();
        reset_realm();
        Guard
    }

    fn wait_result(promise: Value) -> Result<Value, Value> {
        let ok = Rc::new(RefCell::new(None));
        let err = Rc::new(RefCell::new(None));
        let ok_for_then = Rc::clone(&ok);
        let err_for_catch = Rc::clone(&err);
        promise.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *ok_for_then.borrow_mut() = args.first().cloned();
                Value::Undefined
            })],
        );
        promise.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *err_for_catch.borrow_mut() = args.first().cloned();
                Value::Undefined
            })],
        );
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            poll_serial_io();
            crate::jsdom::drain_microtasks();
            if err.borrow().is_some() || ok.borrow().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if let Some(error) = err.borrow().clone() {
            return Err(error);
        }
        ok.borrow()
            .clone()
            .ok_or_else(|| dom_error("NetworkError", "serial operation timed out"))
    }

    fn error_name(value: &Value) -> String {
        value.get_property("name").to_js_string()
    }

    fn filters(vendor: u16, product: Option<u16>) -> Value {
        let mut filter = HashMap::from([("usbVendorId".into(), Value::Number(f64::from(vendor)))]);
        if let Some(product) = product {
            filter.insert("usbProductId".into(), Value::Number(f64::from(product)));
        }
        Value::object(HashMap::from([(
            "filters".into(),
            Value::array(vec![Value::object(filter)]),
        )]))
    }

    struct MockPipe {
        inbound: Mutex<VecDeque<u8>>,
        outbound: Mutex<VecDeque<u8>>,
        lines: Mutex<MockLines>,
    }

    #[derive(Clone, Copy)]
    struct MockLines {
        dtr: bool,
        rts: bool,
        brk: bool,
        dcd: bool,
        cts: bool,
        ri: bool,
        dsr: bool,
    }

    impl Default for MockLines {
        fn default() -> Self {
            Self {
                dtr: false,
                rts: false,
                brk: false,
                dcd: false,
                cts: false,
                ri: false,
                dsr: false,
            }
        }
    }

    struct MockIo {
        pipe: Arc<MockPipe>,
    }

    impl SerialIo for MockIo {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut inbound = lock(&self.pipe.inbound);
            if inbound.is_empty() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "timeout"));
            }
            let count = buf.len().min(inbound.len());
            for slot in buf.iter_mut().take(count) {
                *slot = inbound.pop_front().unwrap_or(0);
            }
            Ok(count)
        }

        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            lock(&self.pipe.outbound).extend(buf.iter().copied());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn set_output_signals(&mut self, signals: &OutputSignals) -> Result<(), String> {
            let mut lines = lock(&self.pipe.lines);
            if let Some(level) = signals.data_terminal_ready {
                lines.dtr = level;
            }
            if let Some(level) = signals.request_to_send {
                lines.rts = level;
            }
            if let Some(level) = signals.break_signal {
                lines.brk = level;
            }
            Ok(())
        }

        fn input_signals(&mut self) -> Result<InputSignals, String> {
            let lines = lock(&self.pipe.lines);
            Ok(InputSignals {
                data_carrier_detect: lines.dcd,
                clear_to_send: lines.cts,
                ring_indicator: lines.ri,
                data_set_ready: lines.dsr,
            })
        }
    }

    struct MockBackend {
        ports: Mutex<Vec<SerialPortRecord>>,
        pipes: Mutex<HashMap<String, Arc<MockPipe>>>,
    }

    impl MockBackend {
        fn new(records: Vec<SerialPortRecord>) -> Arc<Self> {
            let pipes = records
                .iter()
                .map(|record| {
                    (
                        record.name.clone(),
                        Arc::new(MockPipe {
                            inbound: Mutex::new(VecDeque::new()),
                            outbound: Mutex::new(VecDeque::new()),
                            lines: Mutex::new(MockLines::default()),
                        }),
                    )
                })
                .collect();
            Arc::new(Self {
                ports: Mutex::new(records),
                pipes: Mutex::new(pipes),
            })
        }

        fn pipe(&self, name: &str) -> Arc<MockPipe> {
            lock(&self.pipes).get(name).cloned().expect("mock pipe")
        }
    }

    impl SerialBackend for MockBackend {
        fn ports(&self) -> Result<Vec<SerialPortRecord>, String> {
            Ok(lock(&self.ports).clone())
        }

        fn open(&self, name: &str, _options: &OpenSettings) -> Result<Box<dyn SerialIo>, String> {
            let pipes = lock(&self.pipes);
            let Some(pipe) = pipes.get(name).cloned() else {
                return Err(format!("no serial port named {name}"));
            };
            Ok(Box::new(MockIo { pipe }))
        }
    }

    fn install(records: Vec<SerialPortRecord>) -> Arc<MockBackend> {
        let backend = MockBackend::new(records);
        set_backend(Some(backend.clone()));
        backend
    }

    #[test]
    fn missing_adapter_rejects_without_enumerating_a_port() {
        let _guard = setup();
        set_adapter_enabled(false);
        let serial = serial_value();
        assert!(w3cos_core::class::instance_of(&serial, &serial_class()));
        assert!(
            serial_port_class()
                .get_property("prototype")
                .get_property("open")
                .is_function()
        );
        let listed = wait_result(serial.call_method("getPorts", vec![])).expect("ports");
        assert_eq!(listed.get_property("length").to_number(), 0.0);
        let error = wait_result(serial.call_method("requestPort", vec![])).expect_err("request");
        assert_eq!(error_name(&error), "NotFoundError");
        assert!(
            error
                .get_property("message")
                .to_js_string()
                .contains("no Web Serial adapter")
        );
    }

    #[test]
    fn request_requires_a_grant_and_never_selects_silently() {
        let _guard = setup();
        install(vec![SerialPortRecord {
            name: "mock-a".into(),
            usb_vendor_id: Some(0x2341),
            usb_product_id: Some(0x43),
        }]);
        let serial = serial_value();
        let denied = wait_result(serial.call_method("requestPort", vec![])).expect_err("denied");
        assert_eq!(error_name(&denied), "NotAllowedError");
        crate::capability_gate::grant(PERMISSION);
        let silent =
            wait_result(serial.call_method("requestPort", vec![])).expect_err("no chooser");
        assert_eq!(error_name(&silent), "NotFoundError");
        let ports = wait_result(serial.call_method("getPorts", vec![])).expect("ports");
        assert_eq!(ports.get_property("length").to_number(), 0.0);
    }

    #[test]
    fn one_filter_match_grants_info_and_forget_revokes_it() {
        let _guard = setup();
        install(vec![
            SerialPortRecord {
                name: "mock-a".into(),
                usb_vendor_id: Some(0x2341),
                usb_product_id: Some(0x43),
            },
            SerialPortRecord {
                name: "mock-b".into(),
                usb_vendor_id: Some(0x403),
                usb_product_id: Some(0x6001),
            },
        ]);
        crate::capability_gate::grant(PERMISSION);
        let serial = serial_value();
        let missing =
            wait_result(serial.call_method("requestPort", vec![filters(0x1111, Some(0x2222))]))
                .expect_err("missing");
        assert_eq!(error_name(&missing), "NotFoundError");
        let ambiguous = wait_result(serial.call_method(
            "requestPort",
            vec![Value::object(HashMap::from([(
                "filters".into(),
                Value::array(vec![
                    Value::object(HashMap::from([(
                        "usbVendorId".into(),
                        Value::Number(0x2341 as f64),
                    )])),
                    Value::object(HashMap::from([(
                        "usbVendorId".into(),
                        Value::Number(0x403 as f64),
                    )])),
                ]),
            )]))],
        ))
        .expect_err("ambiguous");
        assert_eq!(error_name(&ambiguous), "NotFoundError");
        let empty = wait_result(serial.call_method(
            "requestPort",
            vec![Value::object(HashMap::from([(
                "filters".into(),
                Value::array(vec![Value::object(HashMap::new())]),
            )]))],
        ))
        .expect_err("empty filter");
        assert_eq!(error_name(&empty), "TypeError");

        let port =
            wait_result(serial.call_method("requestPort", vec![filters(0x2341, Some(0x43))]))
                .expect("port");
        assert!(w3cos_core::class::instance_of(&port, &serial_port_class()));
        assert!(port.get_property("connected").to_bool());
        let info = port.call_method("getInfo", vec![]);
        assert_eq!(info.get_property("usbVendorId").to_number(), 0x2341 as f64);
        assert_eq!(info.get_property("usbProductId").to_number(), 0x43 as f64);
        let again = wait_result(serial.call_method("requestPort", vec![filters(0x2341, None)]))
            .expect("same");
        assert!(again.strict_eq(&port));
        let listed = wait_result(serial.call_method("getPorts", vec![])).expect("listed");
        assert_eq!(listed.get_property("length").to_number(), 1.0);
        assert!(listed.get_property("0").strict_eq(&port));
        wait_result(port.call_method("forget", vec![])).expect("forget");
        let after = wait_result(serial.call_method("getPorts", vec![])).expect("after");
        assert_eq!(after.get_property("length").to_number(), 0.0);
    }

    #[test]
    fn chooser_picks_one_port_and_cancel_rejects() {
        let _guard = setup();
        install(vec![
            SerialPortRecord {
                name: "left".into(),
                usb_vendor_id: Some(1),
                usb_product_id: Some(2),
            },
            SerialPortRecord {
                name: "right".into(),
                usb_vendor_id: Some(3),
                usb_product_id: Some(4),
            },
        ]);
        crate::capability_gate::grant(PERMISSION);
        let serial = serial_value();
        set_serial_chooser(Some(Arc::new(|_| None)));
        let cancelled = wait_result(serial.call_method("requestPort", vec![])).expect_err("cancel");
        assert_eq!(error_name(&cancelled), "NotFoundError");
        set_serial_chooser(Some(Arc::new(|ports| {
            ports
                .iter()
                .find(|port| port.name == "right")
                .map(|port| port.name.clone())
        })));
        let port = wait_result(serial.call_method("requestPort", vec![])).expect("chosen");
        assert_eq!(
            port.call_method("getInfo", vec![])
                .get_property("usbVendorId")
                .to_number(),
            3.0
        );
        set_serial_chooser(Some(Arc::new(|_| Some("not-a-port".into()))));
        let outside = wait_result(serial.call_method("requestPort", vec![])).expect_err("outside");
        assert_eq!(error_name(&outside), "NotFoundError");
    }

    #[test]
    fn open_validates_options_and_streams_round_trip_signals() {
        let _guard = setup();
        let backend = install(vec![SerialPortRecord {
            name: "loop".into(),
            usb_vendor_id: Some(9),
            usb_product_id: Some(8),
        }]);
        crate::capability_gate::grant(PERMISSION);
        set_serial_chooser(Some(Arc::new(|ports| {
            ports.first().map(|port| port.name.clone())
        })));
        let serial = serial_value();
        let port = wait_result(serial.call_method("requestPort", vec![])).expect("port");
        for (options, name) in [
            (Value::object(HashMap::new()), "TypeError"),
            (
                Value::object(HashMap::from([("baudRate".into(), Value::Number(0.0))])),
                "TypeError",
            ),
            (
                Value::object(HashMap::from([
                    ("baudRate".into(), Value::Number(9600.0)),
                    ("dataBits".into(), Value::Number(5.0)),
                ])),
                "TypeError",
            ),
            (
                Value::object(HashMap::from([
                    ("baudRate".into(), Value::Number(9600.0)),
                    ("flowControl".into(), Value::string("software")),
                ])),
                "TypeError",
            ),
        ] {
            let error = wait_result(port.call_method("open", vec![options])).expect_err(name);
            assert_eq!(error_name(&error), name, "{name}");
        }
        let closed = wait_result(port.call_method("getSignals", vec![])).expect_err("closed");
        assert_eq!(error_name(&closed), "InvalidStateError");
        wait_result(port.call_method(
            "open",
            vec![Value::object(HashMap::from([
                ("baudRate".into(), Value::Number(115_200.0)),
                ("dataBits".into(), Value::Number(7.0)),
                ("stopBits".into(), Value::Number(2.0)),
                ("parity".into(), Value::string("even")),
                ("bufferSize".into(), Value::Number(32.0)),
                ("flowControl".into(), Value::string("hardware")),
            ]))],
        ))
        .expect("open");
        let again = wait_result(port.call_method(
            "open",
            vec![Value::object(HashMap::from([(
                "baudRate".into(),
                Value::Number(9600.0),
            )]))],
        ))
        .expect_err("reopen");
        assert_eq!(error_name(&again), "InvalidStateError");
        assert!(port.get_property("readable").is_object());
        assert!(w3cos_core::class::instance_of(
            &port.get_property("readable"),
            &crate::streams_web::readable_stream_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &port.get_property("writable"),
            &crate::streams_web::writable_stream_class()
        ));

        let writer = port
            .get_property("writable")
            .call_method("getWriter", vec![]);
        let payload = uint8_array(&[9, 8, 7, 6]);
        wait_result(writer.call_method("write", vec![payload])).expect("write");
        assert_eq!(
            lock(&backend.pipe("loop").outbound)
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![9, 8, 7, 6]
        );

        lock(&backend.pipe("loop").inbound).extend([1, 2, 3, 4]);
        let reader = port
            .get_property("readable")
            .call_method("getReader", vec![]);
        let read = wait_result(reader.call_method("read", vec![])).expect("read");
        assert!(!read.get_property("done").to_bool());
        assert_eq!(
            w3cos_core::binary::bytes_of(&read.get_property("value")).unwrap_or_default(),
            vec![1, 2, 3, 4]
        );

        {
            let pipe = backend.pipe("loop");
            let mut lines = lock(&pipe.lines);
            lines.dcd = true;
            lines.cts = true;
            lines.dsr = true;
        }
        wait_result(port.call_method(
            "setSignals",
            vec![Value::object(HashMap::from([
                ("dataTerminalReady".into(), Value::Bool(true)),
                ("requestToSend".into(), Value::Bool(false)),
                ("break".into(), Value::Bool(true)),
            ]))],
        ))
        .expect("signals");
        let lines = *lock(&backend.pipe("loop").lines);
        assert!(lines.dtr && !lines.rts && lines.brk);
        let input = wait_result(port.call_method("getSignals", vec![])).expect("input");
        assert!(input.get_property("dataCarrierDetect").to_bool());
        assert!(input.get_property("clearToSend").to_bool());
        assert!(!input.get_property("ringIndicator").to_bool());
        assert!(input.get_property("dataSetReady").to_bool());

        wait_result(port.call_method("close", vec![])).expect("close");
        assert!(port.get_property("readable").is_null());
        assert!(port.get_property("writable").is_null());
        let still = wait_result(port.call_method("close", vec![])).expect_err("second close");
        assert_eq!(error_name(&still), "InvalidStateError");
    }

    #[test]
    fn hotplug_dispatches_connect_and_disconnect_for_granted_ports() {
        let _guard = setup();
        let backend = install(vec![SerialPortRecord {
            name: "hot".into(),
            usb_vendor_id: Some(7),
            usb_product_id: Some(1),
        }]);
        crate::capability_gate::grant(PERMISSION);
        set_serial_chooser(Some(Arc::new(|ports| {
            ports.first().map(|port| port.name.clone())
        })));
        let serial = serial_value();
        let events = Rc::new(RefCell::new(Vec::new()));
        let port_events = Rc::clone(&events);
        let manager_events = Rc::clone(&events);
        serial.set_property(
            "ondisconnect",
            Value::function(move |_, _| {
                manager_events
                    .borrow_mut()
                    .push("manager-disconnect".into());
                Value::Undefined
            }),
        );
        serial.set_property(
            "onconnect",
            Value::function(move |_, _| {
                port_events.borrow_mut().push("manager-connect".into());
                Value::Undefined
            }),
        );
        let port = wait_result(serial.call_method("requestPort", vec![])).expect("port");
        let port_log = Rc::clone(&events);
        port.set_property(
            "ondisconnect",
            Value::function(move |_, _| {
                port_log.borrow_mut().push("port-disconnect".into());
                Value::Undefined
            }),
        );
        let port_log = Rc::clone(&events);
        port.set_property(
            "onconnect",
            Value::function(move |_, _| {
                port_log.borrow_mut().push("port-connect".into());
                Value::Undefined
            }),
        );
        assert!(port.get_property("connected").to_bool());
        lock(&backend.ports).clear();
        assert!(refresh_connections(true) >= 1);
        assert!(!port.get_property("connected").to_bool());
        lock(&backend.ports).push(SerialPortRecord {
            name: "hot".into(),
            usb_vendor_id: Some(7),
            usb_product_id: Some(1),
        });
        assert!(refresh_connections(true) >= 1);
        assert!(port.get_property("connected").to_bool());
        let log = events.borrow().clone();
        assert!(log.contains(&"port-disconnect".to_string()));
        assert!(log.contains(&"manager-disconnect".to_string()));
        assert!(log.contains(&"port-connect".to_string()));
        assert!(log.contains(&"manager-connect".to_string()));
    }

    #[cfg(all(feature = "web-serial", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn linux_pty_or_mock_round_trip() {
        let _guard = setup();
        match open_pty_pair() {
            Ok((mut master, path)) => {
                set_serial_ports_override(Some(vec![SerialPortRecord {
                    name: path,
                    usb_vendor_id: None,
                    usb_product_id: None,
                }]));
                crate::capability_gate::grant(PERMISSION);
                set_serial_chooser(Some(Arc::new(|ports| {
                    ports.first().map(|port| port.name.clone())
                })));
                let serial = serial_value();
                let port =
                    wait_result(serial.call_method("requestPort", vec![])).expect("pty port");
                wait_result(port.call_method(
                    "open",
                    vec![Value::object(HashMap::from([(
                        "baudRate".into(),
                        Value::Number(9600.0),
                    )]))],
                ))
                .expect("open pty");
                let writer = port
                    .get_property("writable")
                    .call_method("getWriter", vec![]);
                wait_result(writer.call_method("write", vec![uint8_array(b"ping")]))
                    .expect("write");
                let mut echoed = Vec::new();
                let started = Instant::now();
                while echoed.len() < 4 && started.elapsed() < Duration::from_secs(2) {
                    poll_serial_io();
                    echoed.extend(read_available(&mut master));
                    if echoed.len() < 4 {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                assert_eq!(echoed, b"ping");
                master.write_all(b"pong").expect("master write");
                let reader = port
                    .get_property("readable")
                    .call_method("getReader", vec![]);
                let mut inbound = Vec::new();
                let started = Instant::now();
                while inbound.len() < 4 && started.elapsed() < Duration::from_secs(2) {
                    let read = wait_result(reader.call_method("read", vec![])).expect("read");
                    if read.get_property("done").to_bool() {
                        break;
                    }
                    inbound.extend(
                        w3cos_core::binary::bytes_of(&read.get_property("value"))
                            .unwrap_or_default(),
                    );
                }
                assert_eq!(inbound, b"pong");
                wait_result(port.call_method("close", vec![])).expect("close");
            }
            Err(err) => {
                eprintln!("openpty unavailable ({err}); using in-process serial mock");
                let backend = install(vec![SerialPortRecord {
                    name: "fallback".into(),
                    usb_vendor_id: None,
                    usb_product_id: None,
                }]);
                crate::capability_gate::grant(PERMISSION);
                set_serial_chooser(Some(Arc::new(|ports| {
                    ports.first().map(|port| port.name.clone())
                })));
                let serial = serial_value();
                let port = wait_result(serial.call_method("requestPort", vec![])).expect("mock");
                wait_result(port.call_method(
                    "open",
                    vec![Value::object(HashMap::from([(
                        "baudRate".into(),
                        Value::Number(9600.0),
                    )]))],
                ))
                .expect("open mock");
                let writer = port
                    .get_property("writable")
                    .call_method("getWriter", vec![]);
                wait_result(writer.call_method("write", vec![uint8_array(b"ping")]))
                    .expect("write");
                assert_eq!(
                    lock(&backend.pipe("fallback").outbound)
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    b"ping".to_vec()
                );
                lock(&backend.pipe("fallback").inbound).extend(b"pong".iter().copied());
                let reader = port
                    .get_property("readable")
                    .call_method("getReader", vec![]);
                let read = wait_result(reader.call_method("read", vec![])).expect("read");
                assert_eq!(
                    w3cos_core::binary::bytes_of(&read.get_property("value")).unwrap_or_default(),
                    b"pong".to_vec()
                );
                wait_result(port.call_method("close", vec![])).expect("close");
            }
        }
    }

    #[cfg(all(
        feature = "web-serial",
        any(target_os = "macos", target_os = "linux", target_os = "windows"),
        not(target_env = "ohos")
    ))]
    #[test]
    fn hardware_device_when_enabled() {
        if std::env::var("W3COS_SERIAL_DEVICE_TESTS").ok().as_deref() != Some("1") {
            return;
        }
        let _guard = setup();
        let path = std::env::var("W3COS_SERIAL_DEVICE")
            .unwrap_or_else(|_| panic!("W3COS_SERIAL_DEVICE_TESTS=1 requires W3COS_SERIAL_DEVICE"));
        set_serial_ports_override(Some(vec![SerialPortRecord {
            name: path,
            usb_vendor_id: None,
            usb_product_id: None,
        }]));
        crate::capability_gate::grant(PERMISSION);
        set_serial_chooser(Some(Arc::new(|ports| {
            ports.first().map(|port| port.name.clone())
        })));
        let serial = serial_value();
        let port = wait_result(serial.call_method("requestPort", vec![])).expect("hardware port");
        let info = port.call_method("getInfo", vec![]);
        assert!(info.is_object());
        wait_result(port.call_method(
            "open",
            vec![Value::object(HashMap::from([(
                "baudRate".into(),
                Value::Number(9600.0),
            )]))],
        ))
        .expect("open hardware port");
        wait_result(port.call_method(
            "setSignals",
            vec![Value::object(HashMap::from([(
                "dataTerminalReady".into(),
                Value::Bool(true),
            )]))],
        ))
        .expect("setSignals");
        let signals = wait_result(port.call_method("getSignals", vec![])).expect("getSignals");
        assert!(signals.get_property("clearToSend").is_bool());
        assert!(signals.get_property("dataCarrierDetect").is_bool());
        assert!(signals.get_property("ringIndicator").is_bool());
        assert!(signals.get_property("dataSetReady").is_bool());
        wait_result(port.call_method("close", vec![])).expect("close");
        wait_result(port.call_method("forget", vec![])).expect("forget");
    }

    #[cfg(all(feature = "web-serial", target_os = "linux", not(target_env = "ohos")))]
    fn open_pty_pair() -> Result<(std::fs::File, String), String> {
        use std::os::fd::{AsRawFd, FromRawFd};

        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let mut name = [0i8; 128];
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        unsafe { libc::close(slave) };
        let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if path.is_empty() {
            unsafe { libc::close(master) };
            return Err("openpty returned an empty slave name".into());
        }
        let file = unsafe { std::fs::File::from_raw_fd(master) };
        let fd = file.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Ok((file, path))
    }

    #[cfg(all(feature = "web-serial", target_os = "linux", not(target_env = "ohos")))]
    fn read_available(file: &mut std::fs::File) -> Vec<u8> {
        let mut buffer = [0u8; 256];
        let mut out = Vec::new();
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => out.extend_from_slice(&buffer[..count]),
                Err(err) if is_timeout(&err) => break,
                Err(_) => break,
            }
        }
        out
    }
}
