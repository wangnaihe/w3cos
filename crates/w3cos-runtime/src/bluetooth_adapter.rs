//! Web Bluetooth session: filters, the GATT blocklist, permission, and the
//! desktop radio.
//!
//! `navigator.bluetooth` stays on the standard objects in `bluetooth_web`.
//! This module is the `web-bluetooth` host. macOS, Windows, and Linux desktop
//! builds with `web-bluetooth` talk to a real adapter through `btleplug`
//! (CoreBluetooth, WinRT, BlueZ over D-Bus). Android keeps whatever host
//! module the embedding registered; this module does not replace it.
//!
//! Unit tests use an in-memory radio. A real adapter is touched only outside
//! tests, or when a test opts in with `W3COS_BLUETOOTH_DEVICE_TESTS=1`.
//! Nothing here invents a peripheral when the scan comes back empty.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Mutex;

use w3cos_core::Value;

#[cfg(all(
    feature = "web-bluetooth",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
))]
#[path = "bluetooth_radio.rs"]
mod bluetooth_radio;

const MODULE: &str = "web-bluetooth";
const BASE_UUID_TAIL: &str = "-0000-1000-8000-00805f9b34fb";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockAction {
    Allow,
    Exclude,
    ExcludeReads,
    ExcludeWrites,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothFailure {
    pub name: &'static str,
    pub message: String,
}

impl BluetoothFailure {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CharProps {
    pub broadcast: bool,
    pub read: bool,
    pub write_without_response: bool,
    pub write: bool,
    pub notify: bool,
    pub indicate: bool,
    pub authenticated_signed_writes: bool,
    pub reliable_write: bool,
    pub writable_auxiliaries: bool,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: String,
    pub name: Option<String>,
    pub services: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MockDescriptor {
    pub uuid: String,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct MockCharacteristic {
    pub uuid: String,
    pub properties: CharProps,
    pub value: Vec<u8>,
    pub descriptors: Vec<MockDescriptor>,
}

#[derive(Clone, Debug)]
pub struct MockService {
    pub uuid: String,
    pub primary: bool,
    pub characteristics: Vec<MockCharacteristic>,
}

#[derive(Clone, Debug)]
pub struct MockDevice {
    pub id: String,
    pub name: Option<String>,
    pub advertised_services: Vec<String>,
    pub manufacturer_data: Vec<(u16, Vec<u8>)>,
    pub services: Vec<MockService>,
}

#[derive(Clone, Debug)]
struct MfrFilter {
    company_id: u16,
    prefix: Vec<u8>,
    mask: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Filter {
    services: Vec<String>,
    name: Option<String>,
    name_prefix: Option<String>,
    manufacturer_data: Vec<MfrFilter>,
}

#[derive(Clone, Debug)]
struct Request {
    filters: Vec<Filter>,
    optional_services: Vec<String>,
    accept_all: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Advertisement {
    pub id: String,
    pub name: Option<String>,
    pub services: Vec<String>,
    pub manufacturer_data: HashMap<u16, Vec<u8>>,
}

#[derive(Clone, Debug)]
struct GattDescriptor {
    uuid: String,
}

#[derive(Clone, Debug)]
struct GattCharacteristic {
    uuid: String,
    properties: CharProps,
    descriptors: Vec<GattDescriptor>,
}

#[derive(Clone, Debug)]
struct GattService {
    uuid: String,
    primary: bool,
    characteristics: Vec<GattCharacteristic>,
}

#[derive(Clone, Debug)]
struct Granted {
    id: String,
    name: Option<String>,
    allowed: HashSet<String>,
}

#[derive(Clone, Debug)]
struct LiveChar {
    properties: CharProps,
    value: Vec<u8>,
    descriptors: Vec<MockDescriptor>,
    notifying: bool,
}

#[derive(Clone, Debug)]
struct LiveService {
    primary: bool,
    characteristics: Vec<(String, LiveChar)>,
}

#[derive(Clone, Debug)]
struct LiveDevice {
    name: Option<String>,
    advertised_services: Vec<String>,
    manufacturer_data: HashMap<u16, Vec<u8>>,
    connected: bool,
    services: Vec<(String, LiveService)>,
}

struct Session {
    radio_present: bool,
    devices: HashMap<String, LiveDevice>,
    granted: HashMap<String, Granted>,
    hardware_test: bool,
}

#[derive(Clone, Debug)]
pub enum RadioEvent {
    Disconnected {
        device_id: String,
    },
    Notification {
        device_id: String,
        service_uuid: String,
        characteristic_uuid: String,
        value: Vec<u8>,
    },
}

type Chooser = Rc<dyn Fn(&[Candidate]) -> Option<String>>;

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session {
        radio_present: false,
        devices: HashMap::new(),
        granted: HashMap::new(),
        hardware_test: false,
    });
    static CHOOSER: RefCell<Option<Chooser>> = const { RefCell::new(None) };
    static LOCAL_EVENTS: RefCell<Vec<RadioEvent>> = const { RefCell::new(Vec::new()) };
}

static RADIO_EVENTS: Mutex<Vec<RadioEvent>> = Mutex::new(Vec::new());

fn fail(name: &'static str, message: impl Into<String>) -> BluetoothFailure {
    BluetoothFailure::new(name, message)
}

fn desktop_bluetooth_compiled() -> bool {
    cfg!(all(
        feature = "web-bluetooth",
        any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        ),
        not(target_env = "ohos")
    ))
}

fn backend_is_hardware() -> bool {
    if !desktop_bluetooth_compiled() {
        return false;
    }
    if cfg!(test) {
        return SESSION.with(|session| session.borrow().hardware_test)
            && std::env::var_os("W3COS_BLUETOOTH_DEVICE_TESTS").is_some();
    }
    true
}

pub fn set_hardware_test(enabled: bool) {
    SESSION.with(|session| session.borrow_mut().hardware_test = enabled);
}

pub fn set_chooser(chooser: Option<Chooser>) {
    CHOOSER.with(|slot| *slot.borrow_mut() = chooser);
}

pub fn chooser_installed() -> bool {
    CHOOSER.with(|slot| slot.borrow().is_some())
}

/// Replace the in-memory radio. `present` is adapter presence, not a device.
pub fn set_mock_radio(present: bool, devices: Vec<MockDevice>) {
    SESSION.with(|session| {
        let mut session = session.borrow_mut();
        session.radio_present = present;
        session.devices.clear();
        for device in devices {
            let id = device.id.clone();
            session.devices.insert(id, live_from_mock(device));
        }
    });
}

pub fn on_realm_reset() {
    SESSION.with(|session| {
        let hardware_test = session.borrow().hardware_test;
        *session.borrow_mut() = Session {
            radio_present: false,
            devices: HashMap::new(),
            granted: HashMap::new(),
            hardware_test,
        };
    });
    LOCAL_EVENTS.with(|events| events.borrow_mut().clear());
}

pub fn install_for_tests() {
    on_realm_reset();
    set_chooser(None);
    register_host();
}

pub fn ensure_installed() {
    if w3cos_core::host::has_host_module(MODULE) || !desktop_bluetooth_compiled() {
        return;
    }
    register_host();
}

fn register_host() {
    w3cos_core::host::register_host_module(MODULE, |operation, arguments| {
        let payload = arguments.first().cloned().unwrap_or_default();
        match handle(operation, &payload) {
            Ok(value) => value,
            Err(error) => Value::object(HashMap::from([
                ("error".into(), Value::string(&error.message)),
                ("error_name".into(), Value::string(error.name)),
            ])),
        }
    });
}

pub fn authorize_request() -> Result<(), BluetoothFailure> {
    if crate::capability_gate::is_granted("bluetooth") || chooser_installed() {
        Ok(())
    } else {
        Err(fail(
            "NotAllowedError",
            "bluetooth permission is not granted",
        ))
    }
}

pub fn validate_request(options: &Value) -> Result<(), BluetoothFailure> {
    parse_request(options).map(|_| ())
}

pub fn canonical_assigned(number: u32) -> String {
    format!("{number:08x}{BASE_UUID_TAIL}")
}

pub fn canonical_uuid(kind: Option<&str>, text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("A Bluetooth UUID is required".into());
    }
    if kind == Some("canonicalUUID") {
        return short_uuid(text).ok_or_else(|| format!("Unknown Bluetooth UUID: {text}"));
    }
    if let Some(number) = alias_number(kind, text) {
        return Ok(canonical_assigned(number));
    }
    if let Some(uuid) = short_uuid(text) {
        return Ok(uuid);
    }
    if let Some(uuid) = full_uuid(text) {
        return Ok(uuid);
    }
    Err(format!("Unknown Bluetooth UUID: {text}"))
}

pub fn block_action(uuid: &str) -> BlockAction {
    // https://github.com/WebBluetoothCG/registries/blob/master/gatt_blocklist.txt
    match uuid {
        "00001812-0000-1000-8000-00805f9b34fb"
        | "00001530-1212-efde-1523-785feabcd123"
        | "f000ffc0-0451-4000-b000-000000000000"
        | "00060000-0000-1000-8000-00805f9b34fb"
        | "0000fffd-0000-1000-8000-00805f9b34fb"
        | "0000fff9-0000-1000-8000-00805f9b34fb"
        | "0000fde2-0000-1000-8000-00805f9b34fb"
        | "00002a03-0000-1000-8000-00805f9b34fb"
        | "00002a25-0000-1000-8000-00805f9b34fb" => BlockAction::Exclude,
        "00002a02-0000-1000-8000-00805f9b34fb"
        | "00002902-0000-1000-8000-00805f9b34fb"
        | "00002903-0000-1000-8000-00805f9b34fb" => BlockAction::ExcludeWrites,
        _ => BlockAction::Allow,
    }
}

pub fn take_events() -> Vec<RadioEvent> {
    let mut events = LOCAL_EVENTS.with(|queue| std::mem::take(&mut *queue.borrow_mut()));
    // The process-wide queue belongs to the real radio. Mock tests stay on the
    // thread-local queue so a parallel case cannot consume another thread's events.
    if backend_is_hardware()
        && let Ok(mut radio) = RADIO_EVENTS.lock()
    {
        events.append(&mut radio);
    }
    events
}

pub(crate) fn push_remote_disconnect(device_id: String) {
    if let Ok(mut events) = RADIO_EVENTS.lock() {
        events.push(RadioEvent::Disconnected { device_id });
    }
    crate::uitest::poke_event_loop();
}

pub(crate) fn push_remote_notification(
    device_id: String,
    service_uuid: String,
    characteristic_uuid: String,
    value: Vec<u8>,
) {
    if let Ok(mut events) = RADIO_EVENTS.lock() {
        events.push(RadioEvent::Notification {
            device_id,
            service_uuid,
            characteristic_uuid,
            value,
        });
    }
    crate::uitest::poke_event_loop();
}

fn emit_local(event: RadioEvent) {
    LOCAL_EVENTS.with(|queue| queue.borrow_mut().push(event));
}

pub fn emit_notification(device_id: &str, service: &str, characteristic: &str, value: Vec<u8>) {
    let service = canonical_uuid(None, service).unwrap_or_else(|_| service.to_string());
    let characteristic =
        canonical_uuid(None, characteristic).unwrap_or_else(|_| characteristic.to_string());
    let allowed = SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let Some(device) = session.devices.get_mut(device_id) else {
            return false;
        };
        let Some(ch) = find_live_char_mut(device, &service, &characteristic) else {
            return false;
        };
        if !ch.notifying {
            return false;
        }
        ch.value = value.clone();
        true
    });
    if allowed {
        emit_local(RadioEvent::Notification {
            device_id: device_id.to_string(),
            service_uuid: service,
            characteristic_uuid: characteristic,
            value,
        });
    }
}

fn handle(operation: &str, payload: &Value) -> Result<Value, BluetoothFailure> {
    match operation {
        "get_availability" => Ok(Value::object(HashMap::from([(
            "available".into(),
            Value::Bool(availability()),
        )]))),
        "get_devices" => Ok(devices_response()),
        "request_device" => request_device(payload),
        "connect" => connect(payload),
        "disconnect" => disconnect(payload),
        "get_primary_service" => get_primary_service(payload, false),
        "get_primary_services" => get_primary_service(payload, true),
        "get_characteristic" => get_characteristic(payload, false),
        "get_characteristics" => get_characteristic(payload, true),
        "get_descriptor" => get_descriptor(payload, false),
        "get_descriptors" => get_descriptor(payload, true),
        "read_characteristic" => read_characteristic(payload),
        "write_characteristic" => write_characteristic(payload),
        "start_notifications" => set_notifications(payload, true),
        "stop_notifications" => set_notifications(payload, false),
        "read_descriptor" => read_descriptor(payload),
        "write_descriptor" => write_descriptor(payload),
        _ => Err(fail(
            "NotSupportedError",
            format!("unsupported bluetooth operation {operation}"),
        )),
    }
}

fn availability() -> bool {
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        return bluetooth_radio::available();
    }
    SESSION.with(|session| session.borrow().radio_present)
}

fn devices_response() -> Value {
    let devices = SESSION.with(|session| {
        session
            .borrow()
            .granted
            .values()
            .map(|device| device_response(&device.id, device.name.as_deref()))
            .collect::<Vec<_>>()
    });
    Value::object(HashMap::from([("devices".into(), Value::array(devices))]))
}

fn request_device(options: &Value) -> Result<Value, BluetoothFailure> {
    let request = parse_request(options)?;
    authorize_request()?;
    if !availability() {
        return Err(fail("NotFoundError", "Bluetooth adapter is not available"));
    }
    let advertisements = scan(&request)?;
    let mut matches = Vec::new();
    for advertisement in advertisements {
        if request.accept_all {
            matches.push((advertisement, Vec::new()));
            continue;
        }
        let matched = request
            .filters
            .iter()
            .filter(|filter| filter_matches(filter, &advertisement))
            .cloned()
            .collect::<Vec<_>>();
        if !matched.is_empty() {
            matches.push((advertisement, matched));
        }
    }
    if matches.is_empty() {
        return Err(fail(
            "NotFoundError",
            "no Bluetooth devices matched the request",
        ));
    }
    let index = if chooser_installed() {
        let candidates = matches
            .iter()
            .map(|(advertisement, _)| Candidate {
                id: advertisement.id.clone(),
                name: advertisement.name.clone(),
                services: advertisement.services.clone(),
            })
            .collect::<Vec<_>>();
        let chosen = CHOOSER.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|chooser| chooser(&candidates))
        });
        let Some(id) = chosen else {
            return Err(fail("NotFoundError", "the bluetooth chooser was dismissed"));
        };
        matches
            .iter()
            .position(|(advertisement, _)| advertisement.id == id)
            .ok_or_else(|| fail("NotFoundError", "the chooser returned an unknown device"))?
    } else {
        0
    };
    let (advertisement, filters) = &matches[index];
    let mut allowed = request
        .optional_services
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    if !request.accept_all {
        for filter in filters {
            allowed.extend(filter.services.iter().cloned());
        }
    }
    SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let entry = session
            .granted
            .entry(advertisement.id.clone())
            .or_insert_with(|| Granted {
                id: advertisement.id.clone(),
                name: advertisement.name.clone(),
                allowed: HashSet::new(),
            });
        entry.name = advertisement.name.clone();
        entry.allowed.extend(allowed);
    });
    Ok(device_response(
        &advertisement.id,
        advertisement.name.as_deref(),
    ))
}

fn scan(request: &Request) -> Result<Vec<Advertisement>, BluetoothFailure> {
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        return bluetooth_radio::scan(&request.filters, request.accept_all, chooser_installed());
    }
    let _ = request;
    Ok(SESSION.with(|session| {
        session
            .borrow()
            .devices
            .iter()
            .map(|(id, device)| Advertisement {
                id: id.clone(),
                name: device.name.clone(),
                services: device.advertised_services.clone(),
                manufacturer_data: device.manufacturer_data.clone(),
            })
            .collect()
    }))
}

fn connect(payload: &Value) -> Result<Value, BluetoothFailure> {
    let id = required_string(payload, "device_id", "A device id is required")?;
    require_granted(&id)?;
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        bluetooth_radio::connect(&id)?;
        return Ok(Value::object(HashMap::new()));
    }
    SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let device = session
            .devices
            .get_mut(&id)
            .ok_or_else(|| fail("NetworkError", "Bluetooth device is not in range"))?;
        device.connected = true;
        Ok(Value::object(HashMap::new()))
    })
}

fn disconnect(payload: &Value) -> Result<Value, BluetoothFailure> {
    let id = required_string(payload, "device_id", "A device id is required")?;
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        bluetooth_radio::disconnect(&id)?;
        return Ok(Value::object(HashMap::new()));
    }
    let was_connected = SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let Some(device) = session.devices.get_mut(&id) else {
            return false;
        };
        let connected = device.connected;
        device.connected = false;
        for (_, service) in &mut device.services {
            for (_, characteristic) in &mut service.characteristics {
                characteristic.notifying = false;
            }
        }
        connected
    });
    if was_connected {
        emit_local(RadioEvent::Disconnected { device_id: id });
    }
    Ok(Value::object(HashMap::new()))
}

fn get_primary_service(payload: &Value, all: bool) -> Result<Value, BluetoothFailure> {
    let id = required_string(payload, "device_id", "A device id is required")?;
    let requested = if all {
        optional_uuid(payload, "service_uuid")?
    } else {
        Some(required_uuid(payload, "service_uuid", None)?)
    };
    if let Some(uuid) = &requested {
        reject_blocked_service(uuid)?;
        require_service_allowed(&id, uuid)?;
    }
    let services = load_services(&id)?;
    if all {
        let list = services
            .into_iter()
            .filter(|service| service.primary)
            .filter(|service| block_action(&service.uuid) == BlockAction::Allow)
            .filter(|service| service_allowed(&id, &service.uuid))
            .filter(|service| requested.as_ref().is_none_or(|uuid| uuid == &service.uuid))
            .map(|service| {
                Value::object(HashMap::from([
                    ("uuid".into(), Value::string(&service.uuid)),
                    ("primary".into(), Value::Bool(true)),
                ]))
            })
            .collect::<Vec<_>>();
        return Ok(Value::object(HashMap::from([(
            "services".into(),
            Value::array(list),
        )])));
    }
    let uuid = requested.unwrap_or_default();
    let matches = services
        .iter()
        .filter(|service| service.primary && service.uuid == uuid)
        .count();
    if matches == 0 {
        return Err(fail("NotFoundError", "GATT service not found"));
    }
    if matches > 1 {
        return Err(fail(
            "NotFoundError",
            "multiple GATT services match that UUID",
        ));
    }
    Ok(Value::object(HashMap::from([(
        "uuid".into(),
        Value::string(&uuid),
    )])))
}

fn get_characteristic(payload: &Value, all: bool) -> Result<Value, BluetoothFailure> {
    let id = required_string(payload, "device_id", "A device id is required")?;
    let service_uuid = required_uuid(payload, "service_uuid", None)?;
    reject_blocked_service(&service_uuid)?;
    require_service_allowed(&id, &service_uuid)?;
    let requested = if all {
        optional_uuid(payload, "characteristic_uuid")?
    } else {
        Some(required_uuid(
            payload,
            "characteristic_uuid",
            Some("getCharacteristic"),
        )?)
    };
    if let Some(uuid) = &requested {
        reject_blocked_attribute(uuid, false)?;
    }
    let services = load_services(&id)?;
    let service = unique_service(&services, &service_uuid)?;
    let characteristics = service
        .characteristics
        .iter()
        .filter(|characteristic| {
            requested
                .as_ref()
                .is_none_or(|uuid| uuid == &characteristic.uuid)
        })
        .filter(|characteristic| block_action(&characteristic.uuid) != BlockAction::Exclude)
        .collect::<Vec<_>>();
    if all {
        let list = characteristics
            .into_iter()
            .map(characteristic_response)
            .collect::<Vec<_>>();
        return Ok(Value::object(HashMap::from([(
            "characteristics".into(),
            Value::array(list),
        )])));
    }
    if characteristics.is_empty() {
        return Err(fail("NotFoundError", "GATT characteristic not found"));
    }
    if characteristics.len() > 1 {
        return Err(fail(
            "NotFoundError",
            "multiple GATT characteristics match that UUID",
        ));
    }
    Ok(characteristic_response(characteristics[0]))
}

fn get_descriptor(payload: &Value, all: bool) -> Result<Value, BluetoothFailure> {
    let id = required_string(payload, "device_id", "A device id is required")?;
    let service_uuid = required_uuid(payload, "service_uuid", None)?;
    let characteristic_uuid =
        required_uuid(payload, "characteristic_uuid", Some("getCharacteristic"))?;
    reject_blocked_service(&service_uuid)?;
    require_service_allowed(&id, &service_uuid)?;
    reject_blocked_attribute(&characteristic_uuid, false)?;
    let requested = if all {
        optional_uuid(payload, "descriptor_uuid")?
    } else {
        Some(required_uuid(
            payload,
            "descriptor_uuid",
            Some("getDescriptor"),
        )?)
    };
    if let Some(uuid) = &requested {
        reject_blocked_attribute(uuid, false)?;
    }
    let services = load_services(&id)?;
    let characteristic = unique_characteristic(&services, &service_uuid, &characteristic_uuid)?;
    let descriptors = characteristic
        .descriptors
        .iter()
        .filter(|descriptor| {
            requested
                .as_ref()
                .is_none_or(|uuid| uuid == &descriptor.uuid)
        })
        .filter(|descriptor| block_action(&descriptor.uuid) != BlockAction::Exclude)
        .collect::<Vec<_>>();
    if all {
        let list = descriptors
            .into_iter()
            .map(|descriptor| Value::string(&descriptor.uuid))
            .collect::<Vec<_>>();
        return Ok(Value::object(HashMap::from([(
            "descriptors".into(),
            Value::array(list),
        )])));
    }
    if descriptors.is_empty() {
        return Err(fail("NotFoundError", "GATT descriptor not found"));
    }
    if descriptors.len() > 1 {
        return Err(fail(
            "NotFoundError",
            "multiple GATT descriptors match that UUID",
        ));
    }
    Ok(Value::object(HashMap::from([(
        "uuid".into(),
        Value::string(&descriptors[0].uuid),
    )])))
}

fn read_characteristic(payload: &Value) -> Result<Value, BluetoothFailure> {
    let (id, service, characteristic) = char_path(payload)?;
    reject_blocked_attribute(&characteristic, false)?;
    let props = characteristic_props(&id, &service, &characteristic)?;
    if !props.read {
        return Err(fail(
            "NotSupportedError",
            "GATT characteristic is not readable",
        ));
    }
    if block_action(&characteristic) == BlockAction::ExcludeReads {
        return Err(fail(
            "SecurityError",
            "reading that GATT characteristic is blocked",
        ));
    }
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        let value = bluetooth_radio::read(&id, &service, &characteristic)?;
        return Ok(value_response(&value));
    }
    let value = with_char(&id, &service, &characteristic, |ch| ch.value.clone())?;
    Ok(value_response(&value))
}

fn write_characteristic(payload: &Value) -> Result<Value, BluetoothFailure> {
    let (id, service, characteristic) = char_path(payload)?;
    reject_blocked_attribute(&characteristic, true)?;
    let with_response = payload.get_property("write_type").to_js_string() != "without_response";
    let props = characteristic_props(&id, &service, &characteristic)?;
    if with_response && !props.write {
        return Err(fail(
            "NotSupportedError",
            "GATT characteristic does not support write with response",
        ));
    }
    if !with_response && !props.write_without_response {
        return Err(fail(
            "NotSupportedError",
            "GATT characteristic does not support write without response",
        ));
    }
    let bytes = byte_list(&payload.get_property("value"));
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        bluetooth_radio::write(&id, &service, &characteristic, &bytes, with_response)?;
        return Ok(Value::object(HashMap::new()));
    }
    with_char(&id, &service, &characteristic, |ch| ch.value = bytes)?;
    Ok(Value::object(HashMap::new()))
}

fn set_notifications(payload: &Value, enable: bool) -> Result<Value, BluetoothFailure> {
    let (id, service, characteristic) = char_path(payload)?;
    reject_blocked_attribute(&characteristic, false)?;
    let props = characteristic_props(&id, &service, &characteristic)?;
    if enable && !props.notify && !props.indicate {
        return Err(fail(
            "NotSupportedError",
            "GATT characteristic does not support notifications",
        ));
    }
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        bluetooth_radio::set_notify(&id, &service, &characteristic, enable)?;
        return Ok(Value::object(HashMap::new()));
    }
    with_char(&id, &service, &characteristic, |ch| ch.notifying = enable)?;
    Ok(Value::object(HashMap::new()))
}

fn read_descriptor(payload: &Value) -> Result<Value, BluetoothFailure> {
    let (id, service, characteristic, descriptor) = desc_path(payload)?;
    reject_blocked_attribute(&descriptor, false)?;
    if block_action(&descriptor) == BlockAction::ExcludeReads {
        return Err(fail(
            "SecurityError",
            "reading that GATT descriptor is blocked",
        ));
    }
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        let value = bluetooth_radio::read_descriptor(&id, &service, &characteristic, &descriptor)?;
        return Ok(value_response(&value));
    }
    let value = with_desc(&id, &service, &characteristic, &descriptor, |desc| {
        desc.value.clone()
    })?;
    Ok(value_response(&value))
}

fn write_descriptor(payload: &Value) -> Result<Value, BluetoothFailure> {
    let (id, service, characteristic, descriptor) = desc_path(payload)?;
    reject_blocked_attribute(&descriptor, true)?;
    let bytes = byte_list(&payload.get_property("value"));
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        bluetooth_radio::write_descriptor(&id, &service, &characteristic, &descriptor, &bytes)?;
        return Ok(Value::object(HashMap::new()));
    }
    with_desc(&id, &service, &characteristic, &descriptor, |desc| {
        desc.value = bytes;
    })?;
    Ok(Value::object(HashMap::new()))
}

fn load_services(id: &str) -> Result<Vec<GattService>, BluetoothFailure> {
    require_connected(id)?;
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        return bluetooth_radio::services(id);
    }
    SESSION.with(|session| {
        let session = session.borrow();
        let device = session
            .devices
            .get(id)
            .ok_or_else(|| fail("NetworkError", "GATT server is disconnected"))?;
        Ok(device
            .services
            .iter()
            .map(|(uuid, service)| GattService {
                uuid: uuid.clone(),
                primary: service.primary,
                characteristics: service
                    .characteristics
                    .iter()
                    .map(|(char_uuid, characteristic)| GattCharacteristic {
                        uuid: char_uuid.clone(),
                        properties: characteristic.properties.clone(),
                        descriptors: characteristic
                            .descriptors
                            .iter()
                            .map(|descriptor| GattDescriptor {
                                uuid: descriptor.uuid.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect())
    })
}

fn require_connected(id: &str) -> Result<(), BluetoothFailure> {
    #[cfg(all(
        feature = "web-bluetooth",
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        not(target_env = "ohos")
    ))]
    if backend_is_hardware() {
        return Ok(());
    }
    let connected = SESSION.with(|session| {
        session
            .borrow()
            .devices
            .get(id)
            .is_some_and(|device| device.connected)
    });
    if connected {
        Ok(())
    } else {
        Err(fail("NetworkError", "GATT server is disconnected"))
    }
}

fn require_granted(id: &str) -> Result<(), BluetoothFailure> {
    let granted = SESSION.with(|session| session.borrow().granted.contains_key(id));
    if granted {
        Ok(())
    } else {
        Err(fail(
            "SecurityError",
            "the origin is not allowed to access that Bluetooth device",
        ))
    }
}

fn service_allowed(id: &str, uuid: &str) -> bool {
    SESSION.with(|session| {
        session
            .borrow()
            .granted
            .get(id)
            .is_some_and(|device| device.allowed.contains(uuid))
    })
}

fn require_service_allowed(id: &str, uuid: &str) -> Result<(), BluetoothFailure> {
    if service_allowed(id, uuid) {
        Ok(())
    } else {
        Err(fail(
            "SecurityError",
            "the origin is not allowed to access that GATT service",
        ))
    }
}

fn reject_blocked_service(uuid: &str) -> Result<(), BluetoothFailure> {
    if block_action(uuid) == BlockAction::Exclude {
        Err(fail("SecurityError", "that GATT service is blocked"))
    } else {
        Ok(())
    }
}

fn reject_blocked_attribute(uuid: &str, write: bool) -> Result<(), BluetoothFailure> {
    match (block_action(uuid), write) {
        (BlockAction::Exclude, _) => Err(fail("SecurityError", "that GATT attribute is blocked")),
        (BlockAction::ExcludeWrites, true) => Err(fail(
            "SecurityError",
            "writing that GATT attribute is blocked",
        )),
        (BlockAction::ExcludeReads, false) => Ok(()),
        _ => Ok(()),
    }
}

fn unique_service<'a>(
    services: &'a [GattService],
    uuid: &str,
) -> Result<&'a GattService, BluetoothFailure> {
    let matched = services
        .iter()
        .filter(|service| service.primary && service.uuid == uuid)
        .collect::<Vec<_>>();
    match matched.len() {
        1 => Ok(matched[0]),
        0 => Err(fail("NotFoundError", "GATT service not found")),
        _ => Err(fail(
            "NotFoundError",
            "multiple GATT services match that UUID",
        )),
    }
}

fn unique_characteristic<'a>(
    services: &'a [GattService],
    service_uuid: &str,
    characteristic_uuid: &str,
) -> Result<&'a GattCharacteristic, BluetoothFailure> {
    let service = unique_service(services, service_uuid)?;
    let matched = service
        .characteristics
        .iter()
        .filter(|characteristic| characteristic.uuid == characteristic_uuid)
        .collect::<Vec<_>>();
    match matched.len() {
        1 => Ok(matched[0]),
        0 => Err(fail("NotFoundError", "GATT characteristic not found")),
        _ => Err(fail(
            "NotFoundError",
            "multiple GATT characteristics match that UUID",
        )),
    }
}

fn characteristic_props(
    id: &str,
    service: &str,
    characteristic: &str,
) -> Result<CharProps, BluetoothFailure> {
    let services = load_services(id)?;
    Ok(unique_characteristic(&services, service, characteristic)?
        .properties
        .clone())
}

fn with_char<T>(
    id: &str,
    service: &str,
    characteristic: &str,
    update: impl FnOnce(&mut LiveChar) -> T,
) -> Result<T, BluetoothFailure> {
    require_connected(id)?;
    SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let device = session
            .devices
            .get_mut(id)
            .ok_or_else(|| fail("NetworkError", "GATT server is disconnected"))?;
        let ch = find_live_char_mut(device, service, characteristic)
            .ok_or_else(|| fail("NotFoundError", "GATT characteristic not found"))?;
        Ok(update(ch))
    })
}

fn with_desc<T>(
    id: &str,
    service: &str,
    characteristic: &str,
    descriptor: &str,
    update: impl FnOnce(&mut MockDescriptor) -> T,
) -> Result<T, BluetoothFailure> {
    with_char(id, service, characteristic, |ch| {
        ch.descriptors
            .iter_mut()
            .find(|item| item.uuid == descriptor)
            .map(update)
    })?
    .ok_or_else(|| fail("NotFoundError", "GATT descriptor not found"))
}

fn find_live_char_mut<'a>(
    device: &'a mut LiveDevice,
    service: &str,
    characteristic: &str,
) -> Option<&'a mut LiveChar> {
    device
        .services
        .iter_mut()
        .find(|(uuid, _)| uuid == service)
        .and_then(|(_, service)| {
            service
                .characteristics
                .iter_mut()
                .find(|(uuid, _)| uuid == characteristic)
                .map(|(_, characteristic)| characteristic)
        })
}

fn char_path(payload: &Value) -> Result<(String, String, String), BluetoothFailure> {
    let id = required_string(payload, "device_id", "A device id is required")?;
    let service = required_uuid(payload, "service_uuid", None)?;
    let characteristic = required_uuid(payload, "characteristic_uuid", Some("getCharacteristic"))?;
    reject_blocked_service(&service)?;
    require_service_allowed(&id, &service)?;
    Ok((id, service, characteristic))
}

fn desc_path(payload: &Value) -> Result<(String, String, String, String), BluetoothFailure> {
    let (id, service, characteristic) = char_path(payload)?;
    let descriptor = required_uuid(payload, "descriptor_uuid", Some("getDescriptor"))?;
    Ok((id, service, characteristic, descriptor))
}

fn characteristic_response(characteristic: &GattCharacteristic) -> Value {
    let props = &characteristic.properties;
    Value::object(HashMap::from([
        ("uuid".into(), Value::string(&characteristic.uuid)),
        ("broadcast".into(), Value::Bool(props.broadcast)),
        ("read".into(), Value::Bool(props.read)),
        (
            "writeWithoutResponse".into(),
            Value::Bool(props.write_without_response),
        ),
        ("write".into(), Value::Bool(props.write)),
        ("notify".into(), Value::Bool(props.notify)),
        ("indicate".into(), Value::Bool(props.indicate)),
        (
            "authenticatedSignedWrites".into(),
            Value::Bool(props.authenticated_signed_writes),
        ),
        ("reliableWrite".into(), Value::Bool(props.reliable_write)),
        (
            "writableAuxiliaries".into(),
            Value::Bool(props.writable_auxiliaries),
        ),
    ]))
}

fn value_response(bytes: &[u8]) -> Value {
    Value::object(HashMap::from([(
        "value".into(),
        Value::array(
            bytes
                .iter()
                .copied()
                .map(|byte| Value::Number(byte as f64))
                .collect(),
        ),
    )]))
}

fn device_response(id: &str, name: Option<&str>) -> Value {
    Value::object(HashMap::from([
        ("device_id".into(), Value::string(id)),
        (
            "name".into(),
            match name {
                Some(name) if !name.is_empty() => Value::string(name),
                _ => Value::Null,
            },
        ),
    ]))
}

fn byte_list(value: &Value) -> Vec<u8> {
    value
        .iter()
        .map(|item| item.to_number().clamp(0.0, 255.0) as u8)
        .collect()
}

fn required_string(payload: &Value, key: &str, message: &str) -> Result<String, BluetoothFailure> {
    let text = payload.get_property(key).to_js_string();
    if text.is_empty() || text == "undefined" {
        Err(fail("TypeError", message))
    } else {
        Ok(text)
    }
}

fn required_uuid(
    payload: &Value,
    key: &str,
    kind: Option<&str>,
) -> Result<String, BluetoothFailure> {
    uuid_value(&payload.get_property(key), kind)
}

fn optional_uuid(payload: &Value, key: &str) -> Result<Option<String>, BluetoothFailure> {
    let value = payload.get_property(key);
    if value.is_undefined() || value.is_null() {
        Ok(None)
    } else {
        uuid_value(&value, None).map(Some)
    }
}

fn uuid_value(value: &Value, kind: Option<&str>) -> Result<String, BluetoothFailure> {
    if let Some(number) = value.as_number() {
        if number.is_finite() && number >= 0.0 && number <= u32::MAX as f64 && number.fract() == 0.0
        {
            return Ok(canonical_assigned(number as u32));
        }
        return Err(fail("TypeError", "Bluetooth UUID must be an integer"));
    }
    canonical_uuid(kind, &value.to_js_string()).map_err(|message| fail("TypeError", message))
}

fn parse_request(options: &Value) -> Result<Request, BluetoothFailure> {
    if !options.is_undefined() && !options.is_object() {
        return Err(fail("TypeError", "requestDevice options must be an object"));
    }
    let accept_value = options.get_property("acceptAllDevices");
    let accept_all = if accept_value.is_undefined() {
        false
    } else if let Some(bit) = accept_value.as_bool() {
        bit
    } else {
        return Err(fail("TypeError", "acceptAllDevices must be a boolean"));
    };
    let filters_value = options.get_property("filters");
    let filters_present = !filters_value.is_undefined();
    if accept_all && filters_present {
        return Err(fail(
            "TypeError",
            "requestDevice cannot combine filters with acceptAllDevices",
        ));
    }
    if !accept_all && !filters_present {
        return Err(fail(
            "TypeError",
            "requestDevice requires filters or acceptAllDevices",
        ));
    }
    let mut filters = Vec::new();
    if filters_present {
        if !filters_value.is_array() {
            return Err(fail("TypeError", "filters must be a sequence"));
        }
        for filter in filters_value.iter() {
            filters.push(parse_filter(&filter)?);
        }
        if filters.is_empty() {
            return Err(fail("TypeError", "filters must not be empty"));
        }
    }
    let mut optional_services = Vec::new();
    let optional = options.get_property("optionalServices");
    if !optional.is_undefined() && !optional.is_null() {
        if !optional.is_array() {
            return Err(fail("TypeError", "optionalServices must be a sequence"));
        }
        for service in optional.iter() {
            let uuid = uuid_value(&service, Some("getService"))?;
            reject_blocked_service(&uuid)?;
            optional_services.push(uuid);
        }
    }
    Ok(Request {
        filters,
        optional_services,
        accept_all,
    })
}

fn parse_filter(filter: &Value) -> Result<Filter, BluetoothFailure> {
    if !filter.is_object() {
        return Err(fail(
            "TypeError",
            "a Bluetooth scan filter must be an object",
        ));
    }
    let mut services = Vec::new();
    let services_value = filter.get_property("services");
    if !services_value.is_undefined() {
        if !services_value.is_array() {
            return Err(fail("TypeError", "filter services must be a sequence"));
        }
        for service in services_value.iter() {
            let uuid = uuid_value(&service, Some("getService"))?;
            reject_blocked_service(&uuid)?;
            services.push(uuid);
        }
    }
    let name = optional_text(filter, "name")?;
    let name_prefix = optional_text(filter, "namePrefix")?;
    let mut manufacturer_data = Vec::new();
    let manufacturer = filter.get_property("manufacturerData");
    if !manufacturer.is_undefined() {
        if !manufacturer.is_array() {
            return Err(fail("TypeError", "manufacturerData must be a sequence"));
        }
        for entry in manufacturer.iter() {
            manufacturer_data.push(parse_manufacturer(&entry)?);
        }
    }
    if services.is_empty()
        && name.is_none()
        && name_prefix.is_none()
        && manufacturer_data.is_empty()
    {
        return Err(fail(
            "TypeError",
            "a Bluetooth scan filter must set services, name, namePrefix, or manufacturerData",
        ));
    }
    Ok(Filter {
        services,
        name,
        name_prefix,
        manufacturer_data,
    })
}

fn parse_manufacturer(entry: &Value) -> Result<MfrFilter, BluetoothFailure> {
    let company = entry.get_property("companyIdentifier");
    let Some(number) = company.as_number() else {
        return Err(fail(
            "TypeError",
            "manufacturerData companyIdentifier is required",
        ));
    };
    if !number.is_finite() || number < 0.0 || number > u16::MAX as f64 || number.fract() != 0.0 {
        return Err(fail(
            "TypeError",
            "companyIdentifier must be an unsigned 16-bit integer",
        ));
    }
    let prefix_value = entry.get_property("dataPrefix");
    let prefix = if prefix_value.is_undefined() || prefix_value.is_null() {
        Vec::new()
    } else {
        w3cos_core::binary::bytes_of(&prefix_value).ok_or_else(|| {
            fail(
                "TypeError",
                "manufacturerData dataPrefix must be a BufferSource",
            )
        })?
    };
    let mask_value = entry.get_property("mask");
    let mask = if mask_value.is_undefined() || mask_value.is_null() {
        None
    } else {
        Some(
            w3cos_core::binary::bytes_of(&mask_value)
                .ok_or_else(|| fail("TypeError", "manufacturerData mask must be a BufferSource"))?,
        )
    };
    if let Some(mask) = &mask
        && mask.len() != prefix.len()
    {
        return Err(fail(
            "TypeError",
            "manufacturerData mask length must equal dataPrefix",
        ));
    }
    Ok(MfrFilter {
        company_id: number as u16,
        prefix,
        mask,
    })
}

fn optional_text(value: &Value, key: &str) -> Result<Option<String>, BluetoothFailure> {
    let property = value.get_property(key);
    if property.is_undefined() || property.is_null() {
        return Ok(None);
    }
    if property.as_number().is_some() || property.as_bool().is_some() {
        return Err(fail("TypeError", format!("{key} must be a string")));
    }
    let text = property.to_js_string();
    if text.is_empty() {
        Err(fail("TypeError", format!("{key} must not be empty")))
    } else {
        Ok(Some(text))
    }
}

pub(crate) fn filter_matches(filter: &Filter, advertisement: &Advertisement) -> bool {
    if let Some(name) = &filter.name
        && advertisement.name.as_deref() != Some(name.as_str())
    {
        return false;
    }
    if let Some(prefix) = &filter.name_prefix {
        match &advertisement.name {
            Some(name) if name.starts_with(prefix.as_str()) => {}
            _ => return false,
        }
    }
    if !filter
        .services
        .iter()
        .all(|service| advertisement.services.iter().any(|have| have == service))
    {
        return false;
    }
    for manufacturer in &filter.manufacturer_data {
        let Some(data) = advertisement
            .manufacturer_data
            .get(&manufacturer.company_id)
        else {
            return false;
        };
        if !prefix_matches(data, &manufacturer.prefix, manufacturer.mask.as_deref()) {
            return false;
        }
    }
    true
}

pub(crate) fn scan_satisfied(
    advertisements: &[Advertisement],
    filters: &[Filter],
    accept_all: bool,
) -> bool {
    if accept_all {
        return !advertisements.is_empty();
    }
    advertisements.iter().any(|advertisement| {
        filters
            .iter()
            .any(|filter| filter_matches(filter, advertisement))
    })
}

fn prefix_matches(data: &[u8], prefix: &[u8], mask: Option<&[u8]>) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if data.len() < prefix.len() {
        return false;
    }
    prefix.iter().enumerate().all(|(index, byte)| {
        let mask_byte = mask
            .and_then(|mask| mask.get(index).copied())
            .unwrap_or(0xff);
        data[index] & mask_byte == byte & mask_byte
    })
}

fn live_from_mock(device: MockDevice) -> LiveDevice {
    let mut services = Vec::new();
    for service in device.services {
        let uuid = canonical_uuid(Some("getService"), &service.uuid)
            .unwrap_or_else(|_| service.uuid.to_ascii_lowercase());
        let mut characteristics = Vec::new();
        for characteristic in service.characteristics {
            let char_uuid = canonical_uuid(Some("getCharacteristic"), &characteristic.uuid)
                .unwrap_or_else(|_| characteristic.uuid.to_ascii_lowercase());
            let descriptors = characteristic
                .descriptors
                .into_iter()
                .map(|descriptor| MockDescriptor {
                    uuid: canonical_uuid(Some("getDescriptor"), &descriptor.uuid)
                        .unwrap_or_else(|_| descriptor.uuid.to_ascii_lowercase()),
                    value: descriptor.value,
                })
                .collect();
            characteristics.push((
                char_uuid,
                LiveChar {
                    properties: characteristic.properties,
                    value: characteristic.value,
                    descriptors,
                    notifying: false,
                },
            ));
        }
        services.push((
            uuid,
            LiveService {
                primary: service.primary,
                characteristics,
            },
        ));
    }
    let advertised = device
        .advertised_services
        .iter()
        .map(|uuid| canonical_uuid(Some("getService"), uuid).unwrap_or_else(|_| uuid.clone()))
        .collect();
    LiveDevice {
        name: device.name,
        advertised_services: advertised,
        manufacturer_data: device.manufacturer_data.into_iter().collect(),
        connected: false,
        services,
    }
}

fn alias_number(kind: Option<&str>, name: &str) -> Option<u32> {
    let name = name.to_ascii_lowercase();
    let service = matches!(kind, None | Some("getService"));
    let characteristic = matches!(kind, None | Some("getCharacteristic"));
    let descriptor = matches!(kind, None | Some("getDescriptor"));
    let number = match name.as_str() {
        "generic_access" if service => 0x1800,
        "generic_attribute" if service => 0x1801,
        "immediate_alert" if service => 0x1802,
        "link_loss" if service => 0x1803,
        "tx_power" if service => 0x1804,
        "current_time_service" | "current_time" if service => 0x1805,
        "device_information" if service => 0x180a,
        "heart_rate" if service => 0x180d,
        "battery_service" | "battery" if service => 0x180f,
        "human_interface_device" if service => 0x1812,
        "cycling_power" if service => 0x1818,
        "environmental_sensing" if service => 0x181a,
        "bond_management" if service => 0x181e,
        "gap.device_name" | "device_name" if characteristic => 0x2a00,
        "gap.appearance" | "appearance" if characteristic => 0x2a01,
        "gap.peripheral_privacy_flag" | "peripheral_privacy_flag" if characteristic => 0x2a02,
        "gap.reconnection_address" | "reconnection_address" if characteristic => 0x2a03,
        "gap.peripheral_preferred_connection_parameters" if characteristic => 0x2a04,
        "battery_level" if characteristic => 0x2a19,
        "serial_number_string" if characteristic => 0x2a25,
        "manufacturer_name_string" if characteristic => 0x2a29,
        "heart_rate_measurement" if characteristic => 0x2a37,
        "hid_information" if characteristic => 0x2a4a,
        "report_map" if characteristic => 0x2a4b,
        "report" if characteristic => 0x2a4d,
        "gatt.client_characteristic_configuration" | "client_characteristic_configuration"
            if descriptor =>
        {
            0x2902
        }
        "gatt.server_characteristic_configuration" | "server_characteristic_configuration"
            if descriptor =>
        {
            0x2903
        }
        "gatt.characteristic_user_description" | "characteristic_user_description"
            if descriptor =>
        {
            0x2901
        }
        _ => return None,
    };
    Some(number)
}

fn short_uuid(text: &str) -> Option<String> {
    let hex = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"));
    if let Some(hex) = hex {
        let number = u32::from_str_radix(hex, 16).ok()?;
        return Some(canonical_assigned(number));
    }
    if (text.len() == 4 || text.len() == 8) && text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        let number = u32::from_str_radix(text, 16).ok()?;
        return Some(canonical_assigned(number));
    }
    if text.chars().all(|ch| ch.is_ascii_digit()) {
        let number = text.parse::<u32>().ok()?;
        return Some(canonical_assigned(number));
    }
    None
}

fn full_uuid(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.len() == 32 && text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        let text = text.to_ascii_lowercase();
        return Some(format!(
            "{}-{}-{}-{}-{}",
            &text[0..8],
            &text[8..12],
            &text[12..16],
            &text[16..20],
            &text[20..32]
        ));
    }
    if bytes.len() != 36 {
        return None;
    }
    let ok = bytes.iter().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    });
    ok.then(|| text.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn ad(name: &str, services: &[&str], company: Option<(u16, Vec<u8>)>) -> Advertisement {
        let mut manufacturer_data = HashMap::new();
        if let Some((id, data)) = company {
            manufacturer_data.insert(id, data);
        }
        Advertisement {
            id: "ble:aa".into(),
            name: Some(name.into()),
            services: services
                .iter()
                .map(|service| canonical_uuid(Some("getService"), service).unwrap())
                .collect(),
            manufacturer_data,
        }
    }

    fn services(uuids: &[&str]) -> Filter {
        Filter {
            services: uuids
                .iter()
                .map(|uuid| canonical_uuid(Some("getService"), uuid).unwrap())
                .collect(),
            name: None,
            name_prefix: None,
            manufacturer_data: Vec::new(),
        }
    }

    #[test]
    fn filters_and_within_a_filter_and_or_across_filters() {
        let heart = ad("Tick", &["heart_rate", "battery_service"], None);
        assert!(filter_matches(&services(&["heart_rate"]), &heart));
        assert!(!filter_matches(
            &services(&["heart_rate", "device_information"]),
            &heart
        ));
        let named = Filter {
            name: Some("Tick".into()),
            ..services(&["battery_service"])
        };
        assert!(filter_matches(&named, &heart));
        let prefix = Filter {
            name_prefix: Some("Ti".into()),
            ..services(&[])
        };
        assert!(filter_matches(&prefix, &heart));
        assert!(!filter_matches(
            &Filter {
                name_prefix: Some("Zap".into()),
                ..services(&[])
            },
            &heart
        ));
        let either = [services(&["device_information"]), services(&["heart_rate"])];
        assert!(scan_satisfied(&[heart], &either, false));
        assert!(!scan_satisfied(
            &[ad("Other", &["battery_service"], None)],
            &either,
            false
        ));
    }

    #[test]
    fn manufacturer_data_honors_prefix_and_mask() {
        let device = ad(
            "Band",
            &["battery_service"],
            Some((0x004c, vec![0x10, 0x22, 0x33])),
        );
        let exact = Filter {
            manufacturer_data: vec![MfrFilter {
                company_id: 0x004c,
                prefix: vec![0x10, 0x22],
                mask: None,
            }],
            ..services(&[])
        };
        assert!(filter_matches(&exact, &device));
        let masked = Filter {
            manufacturer_data: vec![MfrFilter {
                company_id: 0x004c,
                prefix: vec![0x10, 0x02],
                mask: Some(vec![0xff, 0x0f]),
            }],
            ..services(&[])
        };
        assert!(filter_matches(&masked, &device));
        let wrong = Filter {
            manufacturer_data: vec![MfrFilter {
                company_id: 0x0006,
                prefix: Vec::new(),
                mask: None,
            }],
            ..services(&[])
        };
        assert!(!filter_matches(&wrong, &device));
    }

    #[test]
    fn blocklist_excludes_hid_serial_and_descriptor_writes() {
        assert_eq!(
            block_action(&canonical_uuid(Some("getService"), "human_interface_device").unwrap()),
            BlockAction::Exclude
        );
        assert_eq!(
            block_action(
                &canonical_uuid(Some("getCharacteristic"), "serial_number_string").unwrap()
            ),
            BlockAction::Exclude
        );
        assert_eq!(
            block_action(
                &canonical_uuid(Some("getDescriptor"), "client_characteristic_configuration")
                    .unwrap()
            ),
            BlockAction::ExcludeWrites
        );
        assert_eq!(
            block_action(&canonical_assigned(0x180f)),
            BlockAction::Allow
        );
        assert_eq!(
            canonical_uuid(None, "180f").unwrap(),
            "0000180f-0000-1000-8000-00805f9b34fb"
        );
    }

    fn error_name(value: &Value) -> String {
        let name = value.get_property("error_name").to_js_string();
        if name == "undefined" {
            String::new()
        } else {
            name
        }
    }

    fn invoke(operation: &str, payload: Value) -> Value {
        w3cos_core::host::invoke(vec![
            Value::string("web-bluetooth"),
            Value::string(operation),
            payload,
        ])
    }

    fn service_filter(service: &str) -> Value {
        Value::object(HashMap::from([(
            "filters".into(),
            Value::array(vec![Value::object(HashMap::from([(
                "services".into(),
                Value::array(vec![Value::string(service)]),
            )]))]),
        )]))
    }

    fn sample_device() -> MockDevice {
        MockDevice {
            id: "ble:aa:bb:cc:dd:ee:ff".into(),
            name: Some("Tick".into()),
            advertised_services: vec!["heart_rate".into(), "battery_service".into()],
            manufacturer_data: vec![(0x004c, vec![0x10, 0x22])],
            services: vec![
                MockService {
                    uuid: "heart_rate".into(),
                    primary: true,
                    characteristics: vec![
                        MockCharacteristic {
                            uuid: "heart_rate_measurement".into(),
                            properties: CharProps {
                                notify: true,
                                read: true,
                                write_without_response: true,
                                ..CharProps::default()
                            },
                            value: vec![9, 8, 7],
                            descriptors: vec![MockDescriptor {
                                uuid: "client_characteristic_configuration".into(),
                                value: vec![0, 0],
                            }],
                        },
                        MockCharacteristic {
                            uuid: "heart_rate_measurement".into(),
                            properties: CharProps {
                                read: true,
                                ..CharProps::default()
                            },
                            value: vec![1],
                            descriptors: Vec::new(),
                        },
                    ],
                },
                MockService {
                    uuid: "battery_service".into(),
                    primary: true,
                    characteristics: vec![MockCharacteristic {
                        uuid: "battery_level".into(),
                        properties: CharProps {
                            read: true,
                            ..CharProps::default()
                        },
                        value: vec![80],
                        descriptors: Vec::new(),
                    }],
                },
                MockService {
                    uuid: "device_information".into(),
                    primary: true,
                    characteristics: vec![MockCharacteristic {
                        uuid: "serial_number_string".into(),
                        properties: CharProps {
                            read: true,
                            ..CharProps::default()
                        },
                        value: vec![1],
                        descriptors: Vec::new(),
                    }],
                },
            ],
        }
    }

    fn gatt_path(service: &str, characteristic: Option<&str>) -> Value {
        let mut fields = HashMap::from([
            ("device_id".into(), Value::string("ble:aa:bb:cc:dd:ee:ff")),
            ("service_uuid".into(), Value::string(service)),
        ]);
        if let Some(characteristic) = characteristic {
            fields.insert("characteristic_uuid".into(), Value::string(characteristic));
        }
        Value::object(fields)
    }

    #[test]
    fn permission_chooser_blocklist_and_mock_gatt_graph() {
        install_for_tests();
        crate::capability_gate::reset();
        set_hardware_test(false);
        set_mock_radio(true, vec![sample_device()]);
        let denied = invoke("request_device", service_filter("heart_rate"));
        assert_eq!(
            denied.get_property("error_name").to_js_string(),
            "NotAllowedError"
        );
        let blocked = invoke("request_device", service_filter("human_interface_device"));
        assert_eq!(
            blocked.get_property("error_name").to_js_string(),
            "SecurityError"
        );
        set_mock_radio(false, vec![]);
        crate::capability_gate::grant("bluetooth");
        let offline = invoke("request_device", service_filter("heart_rate"));
        assert_eq!(
            offline.get_property("error_name").to_js_string(),
            "NotFoundError"
        );
        set_mock_radio(true, vec![sample_device()]);
        crate::capability_gate::reset();
        set_chooser(Some(Rc::new(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate.name.as_deref() == Some("Tick"))
                .map(|candidate| candidate.id.clone())
        })));
        let chosen = invoke("request_device", service_filter("heart_rate"));
        assert_eq!(
            chosen.get_property("device_id").to_js_string(),
            "ble:aa:bb:cc:dd:ee:ff"
        );
        set_chooser(Some(Rc::new(|_| None)));
        let dismissed = invoke("request_device", service_filter("heart_rate"));
        assert_eq!(
            dismissed.get_property("error_name").to_js_string(),
            "NotFoundError"
        );
        set_chooser(None);
        crate::capability_gate::grant("bluetooth");
        let missed = invoke("request_device", service_filter("cycling_power"));
        assert_eq!(
            missed.get_property("error_name").to_js_string(),
            "NotFoundError"
        );
        let device = invoke(
            "request_device",
            Value::object(HashMap::from([
                (
                    "filters".into(),
                    service_filter("heart_rate").get_property("filters"),
                ),
                (
                    "optionalServices".into(),
                    Value::array(vec![Value::string("battery_service")]),
                ),
            ])),
        );
        assert_eq!(device.get_property("name").to_js_string(), "Tick");
        let disconnected = invoke("get_primary_service", gatt_path("heart_rate", None));
        assert_eq!(
            disconnected.get_property("error_name").to_js_string(),
            "NetworkError"
        );
        assert!(
            error_name(&invoke(
                "connect",
                Value::object(HashMap::from([(
                    "device_id".into(),
                    Value::string("ble:aa:bb:cc:dd:ee:ff"),
                )])),
            ))
            .is_empty()
        );
        let foreign = invoke(
            "connect",
            Value::object(HashMap::from([(
                "device_id".into(),
                Value::string("ble:not-granted"),
            )])),
        );
        assert_eq!(
            foreign.get_property("error_name").to_js_string(),
            "SecurityError"
        );
        let hidden = invoke("get_primary_service", gatt_path("device_information", None));
        assert_eq!(
            hidden.get_property("error_name").to_js_string(),
            "SecurityError"
        );
        let battery = invoke("get_primary_service", gatt_path("battery_service", None));
        assert_eq!(
            battery.get_property("uuid").to_js_string(),
            "0000180f-0000-1000-8000-00805f9b34fb"
        );
        let services = invoke(
            "get_primary_services",
            Value::object(HashMap::from([(
                "device_id".into(),
                Value::string("ble:aa:bb:cc:dd:ee:ff"),
            )])),
        );
        assert_eq!(services.get_property("services").iter().count(), 2);
        let ambiguous = invoke(
            "get_characteristic",
            gatt_path("heart_rate", Some("heart_rate_measurement")),
        );
        assert_eq!(
            ambiguous.get_property("error_name").to_js_string(),
            "NotFoundError"
        );
        let serial = invoke(
            "get_characteristic",
            gatt_path("device_information", Some("serial_number_string")),
        );
        assert_eq!(
            serial.get_property("error_name").to_js_string(),
            "SecurityError"
        );
        let level = invoke(
            "get_characteristic",
            gatt_path("battery_service", Some("battery_level")),
        );
        assert!(level.get_property("read").to_bool());
        assert!(!level.get_property("notify").to_bool());
        let reading = invoke(
            "read_characteristic",
            gatt_path("battery_service", Some("battery_level")),
        );
        let bytes = reading
            .get_property("value")
            .iter()
            .map(|item| item.to_number() as u8)
            .collect::<Vec<_>>();
        assert_eq!(bytes, vec![80]);
        let refused = invoke(
            "write_characteristic",
            Value::object(HashMap::from([
                ("device_id".into(), Value::string("ble:aa:bb:cc:dd:ee:ff")),
                ("service_uuid".into(), Value::string("battery_service")),
                ("characteristic_uuid".into(), Value::string("battery_level")),
                ("write_type".into(), Value::string("with_response")),
                ("value".into(), Value::array(vec![Value::Number(1.0)])),
            ])),
        );
        assert_eq!(
            refused.get_property("error_name").to_js_string(),
            "NotSupportedError"
        );
        let ccc = invoke(
            "write_descriptor",
            Value::object(HashMap::from([
                ("device_id".into(), Value::string("ble:aa:bb:cc:dd:ee:ff")),
                ("service_uuid".into(), Value::string("heart_rate")),
                (
                    "characteristic_uuid".into(),
                    Value::string("heart_rate_measurement"),
                ),
                (
                    "descriptor_uuid".into(),
                    Value::string("client_characteristic_configuration"),
                ),
                (
                    "value".into(),
                    Value::array(vec![Value::Number(1.0), Value::Number(0.0)]),
                ),
            ])),
        );
        assert_eq!(
            ccc.get_property("error_name").to_js_string(),
            "SecurityError"
        );
        let duplicate_notify = invoke(
            "start_notifications",
            gatt_path("heart_rate", Some("heart_rate_measurement")),
        );
        assert_eq!(
            duplicate_notify.get_property("error_name").to_js_string(),
            "NotFoundError"
        );
        let listed = invoke("get_devices", Value::object(HashMap::new()));
        assert_eq!(listed.get_property("devices").iter().count(), 1);
        set_chooser(None);
        crate::capability_gate::reset();
    }

    #[test]
    fn notifications_queue_only_after_subscribe_and_disconnect_fires_once() {
        install_for_tests();
        crate::capability_gate::reset();
        crate::capability_gate::grant("bluetooth");
        set_hardware_test(false);
        let mut device = sample_device();
        device.services[0].characteristics.pop();
        set_mock_radio(true, vec![device]);
        assert!(error_name(&invoke("request_device", service_filter("heart_rate"))).is_empty());
        assert!(
            error_name(&invoke(
                "connect",
                Value::object(HashMap::from([(
                    "device_id".into(),
                    Value::string("ble:aa:bb:cc:dd:ee:ff"),
                )])),
            ))
            .is_empty()
        );
        emit_notification(
            "ble:aa:bb:cc:dd:ee:ff",
            "heart_rate",
            "heart_rate_measurement",
            vec![1],
        );
        assert!(take_events().is_empty());
        assert!(
            error_name(&invoke(
                "start_notifications",
                gatt_path("heart_rate", Some("heart_rate_measurement")),
            ))
            .is_empty()
        );
        emit_notification(
            "ble:aa:bb:cc:dd:ee:ff",
            "heart_rate",
            "heart_rate_measurement",
            vec![4, 5],
        );
        match take_events().as_slice() {
            [RadioEvent::Notification { value, .. }] => assert_eq!(value, &vec![4, 5]),
            other => panic!("expected one notification, got {other:?}"),
        }
        assert!(
            error_name(&invoke(
                "disconnect",
                Value::object(HashMap::from([(
                    "device_id".into(),
                    Value::string("ble:aa:bb:cc:dd:ee:ff"),
                )])),
            ))
            .is_empty()
        );
        match take_events().as_slice() {
            [RadioEvent::Disconnected { device_id }] => {
                assert_eq!(device_id, "ble:aa:bb:cc:dd:ee:ff");
            }
            other => panic!("expected one disconnect, got {other:?}"),
        }
        crate::capability_gate::reset();
    }
}
