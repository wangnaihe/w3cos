//! WebUSB platform adapter.
//!
//! Desktop builds with `web-device-access` use `nusb` (pure Rust, no libusb).
//! Android, iOS, and OHOS keep the truthful rejection path. Unit tests install
//! a mock registry. Hardware access is opt-in via `W3COS_USB_DEVICE_TESTS=1`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

/// Interface classes WebUSB forbids `claimInterface` from taking.
/// Audio, HID, mass storage, smart card, video, audio/video, wireless controller.
pub const PROTECTED_INTERFACE_CLASSES: [u8; 7] = [0x01, 0x03, 0x08, 0x0B, 0x0E, 0x10, 0xE0];

thread_local! {
    static MOCK: RefCell<Option<MockRegistry>> = const { RefCell::new(None) };
    static CHOOSER: RefCell<Option<Chooser>> = const { RefCell::new(None) };
    static OPEN: RefCell<HashMap<String, UsbSession>> = RefCell::new(HashMap::new());
    static SEEN: RefCell<Option<HashSet<String>>> = const { RefCell::new(None) };
    static LAST_HOTPLUG: RefCell<Option<Instant>> = const { RefCell::new(None) };
    static LIST_WARNING: RefCell<bool> = const { RefCell::new(false) };
}

type Chooser = Rc<dyn Fn(&[UsbInfo]) -> Option<String>>;

#[derive(Clone, Debug)]
pub struct UsbInfo {
    pub id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub device_version: u16,
    pub usb_version: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    /// Classes visible before a handle is opened. Full endpoint lists live in
    /// `configurations` after `open`.
    pub interface_classes: Vec<InterfaceClass>,
    pub configurations: Vec<UsbConfigurationInfo>,
    pub active_configuration: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct InterfaceClass {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

#[derive(Clone, Debug)]
pub struct UsbConfigurationInfo {
    pub value: u8,
    pub name: Option<String>,
    pub interfaces: Vec<UsbInterfaceInfo>,
}

#[derive(Clone, Debug)]
pub struct UsbInterfaceInfo {
    pub number: u8,
    pub alternates: Vec<UsbAlternateInfo>,
}

#[derive(Clone, Debug)]
pub struct UsbAlternateInfo {
    pub setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub name: Option<String>,
    pub endpoints: Vec<UsbEndpointInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbEndpointKind {
    Bulk,
    Interrupt,
    Isochronous,
}

#[derive(Clone, Debug)]
pub struct UsbEndpointInfo {
    pub address: u8,
    pub packet_size: u16,
    pub kind: UsbEndpointKind,
}

#[derive(Clone, Debug, Default)]
pub struct UsbFilter {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub class_code: Option<u8>,
    pub subclass_code: Option<u8>,
    pub protocol_code: Option<u8>,
    pub serial_number: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlClass {
    Standard,
    Class,
    Vendor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlTarget {
    Device,
    Interface,
    Endpoint,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub struct ControlSetup {
    pub control_class: ControlClass,
    pub recipient: ControlTarget,
    pub request: u8,
    pub value: u16,
    pub index: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferStatus {
    Ok,
    Stall,
    Babble,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InTransfer {
    pub status: TransferStatus,
    pub data: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutTransfer {
    pub status: TransferStatus,
    pub bytes_written: usize,
}

#[derive(Debug)]
pub struct AdapterError {
    pub name: &'static str,
    pub message: String,
}

impl AdapterError {
    pub fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }
}

pub enum UsbChoice {
    NoChooser,
    Dismissed,
    Selected(UsbInfo),
}

pub enum HotplugEvent {
    Connected(UsbInfo),
    Disconnected(String),
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum MockIn {
    Data(Vec<u8>),
    Stall,
    Babble,
    Error(String),
}

#[derive(Clone, Debug)]
pub struct MockUsbSpec {
    pub info: UsbInfo,
    pub inputs: VecDeque<MockIn>,
    pub output_status: VecDeque<TransferStatus>,
}

struct MockEndpoint {
    inputs: std::sync::Arc<std::sync::Mutex<VecDeque<MockIn>>>,
    output_status: std::sync::Arc<std::sync::Mutex<VecDeque<TransferStatus>>>,
    output_log: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

struct MockRegistry {
    devices: Vec<UsbInfo>,
    endpoints: HashMap<String, MockEndpoint>,
}

struct UsbSession {
    info: UsbInfo,
    claimed: HashSet<u8>,
    alternates: HashMap<u8, u8>,
    kind: SessionKind,
}

enum SessionKind {
    Mock(MockEndpoint),
    #[cfg(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    Native {
        device: nusb::Device,
        interfaces: HashMap<u8, nusb::Interface>,
    },
}

pub fn is_protected_interface(class: u8) -> bool {
    PROTECTED_INTERFACE_CLASSES.contains(&class)
}

pub fn platform_compiled() -> bool {
    cfg!(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))
}

pub fn reset() {
    OPEN.with(|open| open.borrow_mut().clear());
    MOCK.with(|mock| *mock.borrow_mut() = None);
    CHOOSER.with(|chooser| *chooser.borrow_mut() = None);
    SEEN.with(|seen| *seen.borrow_mut() = None);
    LAST_HOTPLUG.with(|last| *last.borrow_mut() = None);
    LIST_WARNING.with(|warned| *warned.borrow_mut() = false);
}

pub fn set_chooser(hook: Option<Box<dyn Fn(&[UsbInfo]) -> Option<String>>>) {
    CHOOSER.with(|chooser| {
        *chooser.borrow_mut() = hook.map(|hook| Rc::from(hook) as Chooser);
    });
}

pub fn choose(devices: &[UsbInfo]) -> Result<UsbChoice, AdapterError> {
    let hook = CHOOSER.with(|chooser| chooser.borrow().clone());
    let Some(hook) = hook else {
        return Ok(UsbChoice::NoChooser);
    };
    match hook(devices) {
        Some(id) => devices
            .iter()
            .find(|device| device.id == id)
            .cloned()
            .map(UsbChoice::Selected)
            .ok_or_else(|| {
                AdapterError::new(
                    "NotFoundError",
                    "the USB chooser selected a device that is not connected",
                )
            }),
        None => Ok(UsbChoice::Dismissed),
    }
}

#[allow(dead_code)]
pub fn chooser_installed() -> bool {
    CHOOSER.with(|chooser| chooser.borrow().is_some())
}

pub fn using_mock() -> bool {
    MOCK.with(|mock| mock.borrow().is_some())
}

pub fn set_mock_devices(specs: Vec<MockUsbSpec>) {
    let mut devices = Vec::new();
    let mut endpoints = HashMap::new();
    for spec in specs {
        endpoints.insert(spec.info.id.clone(), mock_endpoint(&spec));
        devices.push(spec.info);
    }
    MOCK.with(|mock| *mock.borrow_mut() = Some(MockRegistry { devices, endpoints }));
}

#[allow(dead_code)]
pub fn push_input(id: &str, result: MockIn) {
    MOCK.with(|mock| {
        if let Some(endpoint) = mock
            .borrow()
            .as_ref()
            .and_then(|registry| registry.endpoints.get(id))
        {
            endpoint
                .inputs
                .lock()
                .expect("usb mock input")
                .push_back(result);
        }
    });
}

pub fn output_log(id: &str) -> Vec<Vec<u8>> {
    MOCK.with(|mock| {
        mock.borrow()
            .as_ref()
            .and_then(|registry| registry.endpoints.get(id))
            .map(|endpoint| endpoint.output_log.lock().expect("usb mock output").clone())
            .unwrap_or_default()
    })
}

fn mock_endpoint(spec: &MockUsbSpec) -> MockEndpoint {
    MockEndpoint {
        inputs: std::sync::Arc::new(std::sync::Mutex::new(spec.inputs.clone())),
        output_status: std::sync::Arc::new(std::sync::Mutex::new(spec.output_status.clone())),
        output_log: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    }
}

pub fn list_devices() -> Vec<UsbInfo> {
    if let Some(devices) = MOCK.with(|mock| {
        mock.borrow()
            .as_ref()
            .map(|registry| registry.devices.clone())
    }) {
        return devices;
    }
    if cfg!(test) && std::env::var_os("W3COS_USB_DEVICE_TESTS").is_none() {
        return Vec::new();
    }
    if !platform_compiled() {
        return Vec::new();
    }
    match platform_list() {
        Ok(devices) => devices,
        Err(err) => {
            let first = LIST_WARNING.with(|warned| {
                let mut warned = warned.borrow_mut();
                let first = !*warned;
                *warned = true;
                first
            });
            if first {
                eprintln!("[w3cos] warning: WebUSB enumeration failed: {err}");
            }
            Vec::new()
        }
    }
}

pub fn backend_summary() -> String {
    if !platform_compiled() {
        return "no-adapter".into();
    }
    match platform_list() {
        Ok(devices) => format!("nusb {} device(s)", devices.len()),
        Err(err) => format!("nusb error: {err}"),
    }
}

pub fn open(id: &str) -> Result<UsbInfo, AdapterError> {
    if let Some(info) = OPEN.with(|open| open.borrow().get(id).map(|session| session.info.clone()))
    {
        return Ok(info);
    }
    let session = if MOCK.with(|mock| mock.borrow().is_some()) {
        open_mock(id)?
    } else {
        open_platform(id)?
    };
    let info = session.info.clone();
    OPEN.with(|open| {
        open.borrow_mut().insert(id.to_string(), session);
    });
    Ok(info)
}

pub fn close(id: &str) {
    OPEN.with(|open| open.borrow_mut().remove(id));
}

pub fn is_open(id: &str) -> bool {
    OPEN.with(|open| open.borrow().contains_key(id))
}

pub fn select_configuration(id: &str, value: u8) -> Result<UsbInfo, AdapterError> {
    with_session_mut(id, |session| {
        if !session.claimed.is_empty() {
            return Err(AdapterError::new(
                "InvalidStateError",
                "release claimed interfaces before selecting a configuration",
            ));
        }
        if !session
            .info
            .configurations
            .iter()
            .any(|configuration| configuration.value == value)
            && !session.info.configurations.is_empty()
        {
            return Err(AdapterError::new(
                "NotFoundError",
                format!("configuration {value} does not exist"),
            ));
        }
        session.select_configuration(value)?;
        session.info.active_configuration = Some(value);
        Ok(session.info.clone())
    })
}

pub fn claim_interface(id: &str, number: u8) -> Result<(), AdapterError> {
    with_session_mut(id, |session| {
        if session.claimed.contains(&number) {
            return Err(AdapterError::new(
                "InvalidStateError",
                format!("interface {number} is already claimed"),
            ));
        }
        let class = session.interface_class(number).ok_or_else(|| {
            AdapterError::new(
                "NotFoundError",
                format!("interface {number} does not exist"),
            )
        })?;
        if is_protected_interface(class) {
            return Err(AdapterError::new(
                "SecurityError",
                format!("interface {number} is a protected USB class 0x{class:02X}"),
            ));
        }
        session.claim(number)?;
        session.claimed.insert(number);
        session.alternates.entry(number).or_insert(0);
        Ok(())
    })
}

pub fn release_interface(id: &str, number: u8) -> Result<(), AdapterError> {
    with_session_mut(id, |session| {
        if !session.claimed.contains(&number) {
            return Err(AdapterError::new(
                "InvalidStateError",
                format!("interface {number} is not claimed"),
            ));
        }
        session.release(number)?;
        session.claimed.remove(&number);
        Ok(())
    })
}

pub fn select_alternate(id: &str, number: u8, setting: u8) -> Result<(), AdapterError> {
    with_session_mut(id, |session| {
        if !session.claimed.contains(&number) {
            return Err(AdapterError::new(
                "InvalidStateError",
                format!("interface {number} is not claimed"),
            ));
        }
        let alternates = session.alternates_of(number).ok_or_else(|| {
            AdapterError::new(
                "NotFoundError",
                format!("interface {number} does not exist"),
            )
        })?;
        if !alternates
            .iter()
            .any(|alternate| alternate.setting == setting)
        {
            return Err(AdapterError::new(
                "NotFoundError",
                format!("alternate setting {setting} does not exist"),
            ));
        }
        session.set_alternate(number, setting)?;
        session.alternates.insert(number, setting);
        Ok(())
    })
}

pub fn control_in(
    id: &str,
    setup: ControlSetup,
    length: usize,
) -> Result<InTransfer, AdapterError> {
    with_session_mut(id, |session| session.control_in(setup, length))
}

pub fn control_out(
    id: &str,
    setup: ControlSetup,
    data: &[u8],
) -> Result<OutTransfer, AdapterError> {
    let data = data.to_vec();
    with_session_mut(id, |session| session.control_out(setup, &data))
}

pub fn transfer_in(id: &str, endpoint: u8, length: usize) -> Result<InTransfer, AdapterError> {
    with_session_mut(id, |session| session.transfer_in(endpoint, length))
}

pub fn transfer_out(id: &str, endpoint: u8, data: &[u8]) -> Result<OutTransfer, AdapterError> {
    let data = data.to_vec();
    with_session_mut(id, |session| session.transfer_out(endpoint, &data))
}

pub fn clear_halt(id: &str, incoming: bool, endpoint: u8) -> Result<(), AdapterError> {
    with_session_mut(id, |session| session.clear_halt(incoming, endpoint))
}

pub fn reset_device(id: &str) -> Result<UsbInfo, AdapterError> {
    with_session_mut(id, |session| {
        session.reset_device()?;
        session.claimed.clear();
        Ok(session.info.clone())
    })
}

pub fn poll_hotplug(allowed: &HashSet<String>) -> Vec<HotplugEvent> {
    if !cfg!(test) {
        let due = LAST_HOTPLUG.with(|last| {
            last.borrow()
                .map(|instant| instant.elapsed() >= Duration::from_millis(400))
                .unwrap_or(true)
        });
        if !due {
            return Vec::new();
        }
    }
    LAST_HOTPLUG.with(|last| *last.borrow_mut() = Some(Instant::now()));
    let present = list_devices();
    let ids: HashSet<String> = present.iter().map(|device| device.id.clone()).collect();
    let previous = SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.is_none() {
            *seen = Some(ids.clone());
            return None;
        }
        seen.clone()
    });
    let Some(previous) = previous else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for device in &present {
        if !previous.contains(&device.id) && allowed.contains(&device.id) {
            events.push(HotplugEvent::Connected(device.clone()));
        }
    }
    for id in previous.difference(&ids) {
        if allowed.contains(id) {
            close(id);
            events.push(HotplugEvent::Disconnected(id.clone()));
        }
    }
    SEEN.with(|seen| *seen.borrow_mut() = Some(ids));
    events
}

impl UsbInfo {
    pub fn matches(&self, filter: &UsbFilter) -> bool {
        if filter.vendor_id.is_some_and(|id| id != self.vendor_id) {
            return false;
        }
        if filter.product_id.is_some_and(|id| id != self.product_id) {
            return false;
        }
        if filter
            .serial_number
            .as_ref()
            .is_some_and(|serial| Some(serial) != self.serial.as_ref())
        {
            return false;
        }
        let Some(class_code) = filter.class_code else {
            return true;
        };
        let device_level = self.device_class != 0
            && self.device_class == class_code
            && filter
                .subclass_code
                .is_none_or(|subclass| subclass == self.device_subclass)
            && filter
                .protocol_code
                .is_none_or(|protocol| protocol == self.device_protocol);
        if device_level {
            return true;
        }
        self.class_triples().any(|triple| {
            triple.class == class_code
                && filter
                    .subclass_code
                    .is_none_or(|subclass| subclass == triple.subclass)
                && filter
                    .protocol_code
                    .is_none_or(|protocol| protocol == triple.protocol)
        })
    }

    fn class_triples(&self) -> impl Iterator<Item = InterfaceClass> + '_ {
        let from_configurations = self.configurations.iter().flat_map(|configuration| {
            configuration.interfaces.iter().filter_map(|interface| {
                interface
                    .alternates
                    .first()
                    .map(|alternate| InterfaceClass {
                        number: interface.number,
                        class: alternate.class,
                        subclass: alternate.subclass,
                        protocol: alternate.protocol,
                    })
            })
        });
        from_configurations.chain(self.interface_classes.iter().cloned())
    }
}

fn with_session_mut<T>(
    id: &str,
    body: impl FnOnce(&mut UsbSession) -> Result<T, AdapterError>,
) -> Result<T, AdapterError> {
    OPEN.with(|open| {
        let mut open = open.borrow_mut();
        let Some(session) = open.get_mut(id) else {
            return Err(AdapterError::new(
                "InvalidStateError",
                "USBDevice is not open",
            ));
        };
        body(session)
    })
}

fn open_mock(id: &str) -> Result<UsbSession, AdapterError> {
    let found = MOCK.with(|mock| {
        mock.borrow().as_ref().and_then(|registry| {
            registry
                .devices
                .iter()
                .find(|device| device.id == id)
                .cloned()
                .zip(registry.endpoints.get(id).map(|endpoint| MockEndpoint {
                    inputs: std::sync::Arc::clone(&endpoint.inputs),
                    output_status: std::sync::Arc::clone(&endpoint.output_status),
                    output_log: std::sync::Arc::clone(&endpoint.output_log),
                }))
        })
    });
    let Some((info, endpoint)) = found else {
        return Err(AdapterError::new(
            "NotFoundError",
            "USB device is no longer connected",
        ));
    };
    Ok(UsbSession {
        info,
        claimed: HashSet::new(),
        alternates: HashMap::new(),
        kind: SessionKind::Mock(endpoint),
    })
}

fn open_platform(id: &str) -> Result<UsbSession, AdapterError> {
    #[cfg(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    {
        return native::open(id);
    }
    #[cfg(not(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    )))]
    {
        let _ = id;
        Err(AdapterError::new(
            "NotFoundError",
            "this build has no WebUSB adapter",
        ))
    }
}

fn platform_list() -> Result<Vec<UsbInfo>, String> {
    #[cfg(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    {
        return native::list();
    }
    #[cfg(not(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    )))]
    {
        Err("this build has no WebUSB adapter".into())
    }
}

impl UsbSession {
    fn interface_class(&self, number: u8) -> Option<u8> {
        let setting = self.alternates.get(&number).copied().unwrap_or(0);
        self.alternates_of(number)
            .and_then(|alternates| {
                alternates
                    .iter()
                    .find(|alternate| alternate.setting == setting)
                    .or_else(|| alternates.first())
                    .map(|alternate| alternate.class)
            })
            .or_else(|| {
                self.info
                    .interface_classes
                    .iter()
                    .find(|interface| interface.number == number)
                    .map(|interface| interface.class)
            })
    }

    fn alternates_of(&self, number: u8) -> Option<&[UsbAlternateInfo]> {
        self.info.configurations.iter().find_map(|configuration| {
            configuration
                .interfaces
                .iter()
                .find(|interface| interface.number == number)
                .map(|interface| interface.alternates.as_slice())
        })
    }

    fn endpoint(&self, number: u8, incoming: bool) -> Result<(u8, UsbEndpointInfo), AdapterError> {
        let address = if incoming {
            number | 0x80
        } else {
            number & 0x7F
        };
        let active = self.info.active_configuration;
        for configuration in &self.info.configurations {
            if active.is_some_and(|value| value != configuration.value) {
                continue;
            }
            for interface in &configuration.interfaces {
                let setting = self.alternates.get(&interface.number).copied().unwrap_or(0);
                let Some(alternate) = interface
                    .alternates
                    .iter()
                    .find(|alternate| alternate.setting == setting)
                    .or_else(|| interface.alternates.first())
                else {
                    continue;
                };
                if let Some(endpoint) = alternate
                    .endpoints
                    .iter()
                    .find(|endpoint| endpoint.address == address)
                {
                    return Ok((interface.number, endpoint.clone()));
                }
            }
        }
        Err(AdapterError::new(
            "NotFoundError",
            format!("endpoint {number} was not found"),
        ))
    }

    fn select_configuration(&mut self, value: u8) -> Result<(), AdapterError> {
        let refreshed = match &mut self.kind {
            SessionKind::Mock(_) => None,
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { device, .. } => {
                use nusb::MaybeFuture;
                device
                    .set_configuration(value)
                    .wait()
                    .map_err(native::map_error)?;
                native::snapshot(device)
            }
        };
        if let Some(configurations) = refreshed {
            self.info.configurations = configurations;
        }
        Ok(())
    }

    fn claim(&mut self, number: u8) -> Result<(), AdapterError> {
        match &mut self.kind {
            SessionKind::Mock(_) => Ok(()),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { device, interfaces } => {
                use nusb::MaybeFuture;
                let interface = device
                    .claim_interface(number)
                    .wait()
                    .map_err(native::map_error)?;
                interfaces.insert(number, interface);
                Ok(())
            }
        }
    }

    fn release(&mut self, number: u8) -> Result<(), AdapterError> {
        match &mut self.kind {
            SessionKind::Mock(_) => Ok(()),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { interfaces, .. } => {
                use nusb::MaybeFuture;
                if let Some(interface) = interfaces.remove(&number) {
                    interface.release().wait().map_err(native::map_error)?;
                }
                Ok(())
            }
        }
    }

    fn set_alternate(&mut self, number: u8, setting: u8) -> Result<(), AdapterError> {
        match &mut self.kind {
            SessionKind::Mock(_) => Ok(()),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { interfaces, .. } => {
                use nusb::MaybeFuture;
                let Some(interface) = interfaces.get(&number) else {
                    return Err(AdapterError::new(
                        "InvalidStateError",
                        format!("interface {number} is not claimed"),
                    ));
                };
                interface
                    .set_alt_setting(setting)
                    .wait()
                    .map_err(native::map_error)?;
                Ok(())
            }
        }
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        length: usize,
    ) -> Result<InTransfer, AdapterError> {
        match &self.kind {
            SessionKind::Mock(endpoint) => {
                let _ = (setup, length);
                endpoint.next_in()
            }
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { device, interfaces } => {
                native::control_in(device, interfaces, setup, length)
            }
        }
    }

    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
    ) -> Result<OutTransfer, AdapterError> {
        match &self.kind {
            SessionKind::Mock(endpoint) => {
                let _ = setup;
                endpoint.next_out(data)
            }
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { device, interfaces } => {
                native::control_out(device, interfaces, setup, data)
            }
        }
    }

    fn transfer_in(
        &mut self,
        endpoint_number: u8,
        length: usize,
    ) -> Result<InTransfer, AdapterError> {
        let (interface_number, endpoint) = self.endpoint(endpoint_number, true)?;
        if !self.claimed.contains(&interface_number) {
            return Err(AdapterError::new(
                "InvalidStateError",
                format!("interface {interface_number} is not claimed"),
            ));
        }
        if endpoint.kind == UsbEndpointKind::Isochronous {
            return Err(isochronous_error());
        }
        match &self.kind {
            SessionKind::Mock(endpoint) => endpoint.next_in(),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { interfaces, .. } => {
                let Some(interface) = interfaces.get(&interface_number) else {
                    return Err(AdapterError::new(
                        "InvalidStateError",
                        format!("interface {interface_number} is not claimed"),
                    ));
                };
                native::transfer_in(interface, &endpoint, length)
            }
        }
    }

    fn transfer_out(
        &mut self,
        endpoint_number: u8,
        data: &[u8],
    ) -> Result<OutTransfer, AdapterError> {
        let (interface_number, endpoint) = self.endpoint(endpoint_number, false)?;
        if !self.claimed.contains(&interface_number) {
            return Err(AdapterError::new(
                "InvalidStateError",
                format!("interface {interface_number} is not claimed"),
            ));
        }
        if endpoint.kind == UsbEndpointKind::Isochronous {
            return Err(isochronous_error());
        }
        match &self.kind {
            SessionKind::Mock(mock) => mock.next_out(data),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { interfaces, .. } => {
                let Some(interface) = interfaces.get(&interface_number) else {
                    return Err(AdapterError::new(
                        "InvalidStateError",
                        format!("interface {interface_number} is not claimed"),
                    ));
                };
                native::transfer_out(interface, &endpoint, data)
            }
        }
    }

    fn clear_halt(&mut self, incoming: bool, endpoint_number: u8) -> Result<(), AdapterError> {
        let (interface_number, endpoint) = self.endpoint(endpoint_number, incoming)?;
        if endpoint.kind == UsbEndpointKind::Isochronous {
            return Err(isochronous_error());
        }
        if !self.claimed.contains(&interface_number) {
            return Err(AdapterError::new(
                "InvalidStateError",
                format!("interface {interface_number} is not claimed"),
            ));
        }
        match &self.kind {
            SessionKind::Mock(_) => Ok(()),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { interfaces, .. } => {
                let Some(interface) = interfaces.get(&interface_number) else {
                    return Err(AdapterError::new(
                        "InvalidStateError",
                        format!("interface {interface_number} is not claimed"),
                    ));
                };
                native::clear_halt(interface, &endpoint)
            }
        }
    }

    fn reset_device(&mut self) -> Result<(), AdapterError> {
        let active = match &mut self.kind {
            SessionKind::Mock(_) => return Ok(()),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { device, interfaces } => {
                use nusb::MaybeFuture;
                interfaces.clear();
                device.reset().wait().map_err(native::map_error)?;
                device
                    .active_configuration()
                    .ok()
                    .map(|configuration| configuration.configuration_value())
            }
        };
        self.info.active_configuration = active;
        Ok(())
    }
}

fn isochronous_error() -> AdapterError {
    AdapterError::new(
        "NotSupportedError",
        "WebUSB isochronous transfers are not implemented",
    )
}

impl MockEndpoint {
    fn next_in(&self) -> Result<InTransfer, AdapterError> {
        let scripted = self.inputs.lock().expect("usb mock input").pop_front();
        match scripted {
            Some(MockIn::Data(data)) => Ok(InTransfer {
                status: TransferStatus::Ok,
                data: Some(data),
            }),
            Some(MockIn::Stall) => Ok(InTransfer {
                status: TransferStatus::Stall,
                data: None,
            }),
            Some(MockIn::Babble) => Ok(InTransfer {
                status: TransferStatus::Babble,
                data: None,
            }),
            Some(MockIn::Error(message)) => Err(AdapterError::new("NetworkError", message)),
            None => Ok(InTransfer {
                status: TransferStatus::Ok,
                data: Some(Vec::new()),
            }),
        }
    }

    fn next_out(&self, data: &[u8]) -> Result<OutTransfer, AdapterError> {
        self.output_log
            .lock()
            .expect("usb mock output")
            .push(data.to_vec());
        let status = self
            .output_status
            .lock()
            .expect("usb mock status")
            .pop_front()
            .unwrap_or(TransferStatus::Ok);
        Ok(OutTransfer {
            bytes_written: if status == TransferStatus::Ok {
                data.len()
            } else {
                0
            },
            status,
        })
    }
}

#[cfg(all(
    feature = "web-device-access",
    any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    )
))]
mod native {
    use super::*;
    use nusb::MaybeFuture;
    use nusb::descriptors::TransferType;
    use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Interrupt, Out, Recipient};

    pub fn list() -> Result<Vec<UsbInfo>, String> {
        let devices = nusb::list_devices().wait().map_err(|err| err.to_string())?;
        Ok(devices.map(|device| info_from_device(&device)).collect())
    }

    pub fn open(id: &str) -> Result<UsbSession, AdapterError> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|err| AdapterError::new("NetworkError", err.to_string()))?;
        for device_info in devices {
            if device_key(&device_info) != id {
                continue;
            }
            let mut info = info_from_device(&device_info);
            let device = device_info
                .open()
                .wait()
                .map_err(|err| AdapterError::new("NetworkError", err.to_string()))?;
            if let Some(configurations) = snapshot(&device) {
                info.configurations = configurations;
                info.interface_classes = classes_from_configurations(&info.configurations);
            }
            info.active_configuration = device
                .active_configuration()
                .ok()
                .map(|configuration| configuration.configuration_value());
            return Ok(UsbSession {
                info,
                claimed: HashSet::new(),
                alternates: HashMap::new(),
                kind: SessionKind::Native {
                    device,
                    interfaces: HashMap::new(),
                },
            });
        }
        Err(AdapterError::new(
            "NotFoundError",
            "USB device is no longer connected",
        ))
    }

    pub fn snapshot(device: &nusb::Device) -> Option<Vec<UsbConfigurationInfo>> {
        let configurations: Vec<_> = device
            .configurations()
            .map(|configuration| UsbConfigurationInfo {
                value: configuration.configuration_value(),
                name: None,
                interfaces: configuration
                    .interfaces()
                    .map(|interface| UsbInterfaceInfo {
                        number: interface.interface_number(),
                        alternates: interface
                            .alt_settings()
                            .map(|alternate| UsbAlternateInfo {
                                setting: alternate.alternate_setting(),
                                class: alternate.class(),
                                subclass: alternate.subclass(),
                                protocol: alternate.protocol(),
                                name: None,
                                endpoints: alternate
                                    .endpoints()
                                    .map(|endpoint| UsbEndpointInfo {
                                        address: endpoint.address(),
                                        packet_size: u16::try_from(endpoint.max_packet_size())
                                            .unwrap_or(u16::MAX),
                                        kind: match endpoint.transfer_type() {
                                            TransferType::Interrupt => UsbEndpointKind::Interrupt,
                                            TransferType::Isochronous => {
                                                UsbEndpointKind::Isochronous
                                            }
                                            _ => UsbEndpointKind::Bulk,
                                        },
                                    })
                                    .collect(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect();
        (!configurations.is_empty()).then_some(configurations)
    }

    pub fn control_in(
        device: &nusb::Device,
        interfaces: &HashMap<u8, nusb::Interface>,
        setup: ControlSetup,
        length: usize,
    ) -> Result<InTransfer, AdapterError> {
        let control = ControlIn {
            control_type: control_type(setup.control_class),
            recipient: recipient(setup.recipient),
            request: setup.request,
            value: setup.value,
            index: setup.index,
            length: u16::try_from(length).unwrap_or(u16::MAX),
        };
        let result = dispatch_control_in(device, interfaces, control);
        map_in_result(result)
    }

    pub fn control_out(
        device: &nusb::Device,
        interfaces: &HashMap<u8, nusb::Interface>,
        setup: ControlSetup,
        data: &[u8],
    ) -> Result<OutTransfer, AdapterError> {
        let control = ControlOut {
            control_type: control_type(setup.control_class),
            recipient: recipient(setup.recipient),
            request: setup.request,
            value: setup.value,
            index: setup.index,
            data,
        };
        map_out_result(
            dispatch_control_out(device, interfaces, control),
            data.len(),
        )
    }

    pub fn transfer_in(
        interface: &nusb::Interface,
        endpoint: &UsbEndpointInfo,
        length: usize,
    ) -> Result<InTransfer, AdapterError> {
        if length == 0 {
            return Ok(InTransfer {
                status: TransferStatus::Ok,
                data: Some(Vec::new()),
            });
        }
        let packet = usize::from(endpoint.packet_size).max(1);
        let padded = length.div_ceil(packet) * packet;
        let buffer = nusb::transfer::Buffer::new(padded);
        let completion = match endpoint.kind {
            UsbEndpointKind::Interrupt => {
                let mut opened = interface
                    .endpoint::<Interrupt, In>(endpoint.address)
                    .map_err(map_error)?;
                opened.transfer_blocking(buffer, TRANSFER_TIMEOUT)
            }
            UsbEndpointKind::Bulk => {
                let mut opened = interface
                    .endpoint::<Bulk, In>(endpoint.address)
                    .map_err(map_error)?;
                opened.transfer_blocking(buffer, TRANSFER_TIMEOUT)
            }
            UsbEndpointKind::Isochronous => return Err(super::isochronous_error()),
        };
        let status = completion.status;
        let actual = completion.actual_len.min(length);
        let bytes = completion.buffer.into_vec();
        let data = bytes.get(..actual.min(bytes.len())).unwrap_or(&[]).to_vec();
        completion_in(status, data)
    }

    pub fn transfer_out(
        interface: &nusb::Interface,
        endpoint: &UsbEndpointInfo,
        data: &[u8],
    ) -> Result<OutTransfer, AdapterError> {
        let buffer = nusb::transfer::Buffer::from(data.to_vec());
        let completion = match endpoint.kind {
            UsbEndpointKind::Interrupt => {
                let mut opened = interface
                    .endpoint::<Interrupt, Out>(endpoint.address)
                    .map_err(map_error)?;
                opened.transfer_blocking(buffer, TRANSFER_TIMEOUT)
            }
            UsbEndpointKind::Bulk => {
                let mut opened = interface
                    .endpoint::<Bulk, Out>(endpoint.address)
                    .map_err(map_error)?;
                opened.transfer_blocking(buffer, TRANSFER_TIMEOUT)
            }
            UsbEndpointKind::Isochronous => return Err(super::isochronous_error()),
        };
        let status = completion.status;
        let actual = completion.actual_len;
        completion_out(status, actual, data.len())
    }

    pub fn clear_halt(
        interface: &nusb::Interface,
        endpoint: &UsbEndpointInfo,
    ) -> Result<(), AdapterError> {
        let incoming = endpoint.address & 0x80 != 0;
        let cleared = match (endpoint.kind, incoming) {
            (UsbEndpointKind::Interrupt, true) => interface
                .endpoint::<Interrupt, In>(endpoint.address)
                .map_err(map_error)?
                .clear_halt()
                .wait(),
            (UsbEndpointKind::Interrupt, false) => interface
                .endpoint::<Interrupt, Out>(endpoint.address)
                .map_err(map_error)?
                .clear_halt()
                .wait(),
            (UsbEndpointKind::Bulk, true) => interface
                .endpoint::<Bulk, In>(endpoint.address)
                .map_err(map_error)?
                .clear_halt()
                .wait(),
            (UsbEndpointKind::Bulk, false) => interface
                .endpoint::<Bulk, Out>(endpoint.address)
                .map_err(map_error)?
                .clear_halt()
                .wait(),
            (UsbEndpointKind::Isochronous, _) => return Err(super::isochronous_error()),
        };
        cleared.map_err(map_error)
    }

    pub fn map_error(err: nusb::Error) -> AdapterError {
        let name = match err.kind() {
            nusb::ErrorKind::PermissionDenied => "NotAllowedError",
            nusb::ErrorKind::NotFound => "NotFoundError",
            nusb::ErrorKind::Unsupported => "NotSupportedError",
            nusb::ErrorKind::Disconnected | nusb::ErrorKind::Busy | nusb::ErrorKind::Other => {
                "NetworkError"
            }
            _ => "NetworkError",
        };
        AdapterError::new(name, err.to_string())
    }

    fn info_from_device(device: &nusb::DeviceInfo) -> UsbInfo {
        let interface_classes = device
            .interfaces()
            .map(|interface| InterfaceClass {
                number: interface.interface_number(),
                class: interface.class(),
                subclass: interface.subclass(),
                protocol: interface.protocol(),
            })
            .collect();
        UsbInfo {
            id: device_key(device),
            vendor_id: device.vendor_id(),
            product_id: device.product_id(),
            device_class: device.class(),
            device_subclass: device.subclass(),
            device_protocol: device.protocol(),
            device_version: device.device_version(),
            usb_version: device.usb_version(),
            manufacturer: device.manufacturer_string().map(str::to_string),
            product: device.product_string().map(str::to_string),
            serial: device.serial_number().map(str::to_string),
            interface_classes,
            configurations: Vec::new(),
            active_configuration: None,
        }
    }

    fn device_key(device: &nusb::DeviceInfo) -> String {
        format!(
            "{}:{}:{:04x}:{:04x}:{}",
            device.bus_id(),
            device.device_address(),
            device.vendor_id(),
            device.product_id(),
            device.serial_number().unwrap_or("")
        )
    }

    fn classes_from_configurations(configurations: &[UsbConfigurationInfo]) -> Vec<InterfaceClass> {
        configurations
            .iter()
            .flat_map(|configuration| {
                configuration.interfaces.iter().filter_map(|interface| {
                    interface
                        .alternates
                        .first()
                        .map(|alternate| InterfaceClass {
                            number: interface.number,
                            class: alternate.class,
                            subclass: alternate.subclass,
                            protocol: alternate.protocol,
                        })
                })
            })
            .collect()
    }

    fn control_type(class: ControlClass) -> ControlType {
        match class {
            ControlClass::Standard => ControlType::Standard,
            ControlClass::Class => ControlType::Class,
            ControlClass::Vendor => ControlType::Vendor,
        }
    }

    fn recipient(target: ControlTarget) -> Recipient {
        match target {
            ControlTarget::Device => Recipient::Device,
            ControlTarget::Interface => Recipient::Interface,
            ControlTarget::Endpoint => Recipient::Endpoint,
            ControlTarget::Other => Recipient::Other,
        }
    }

    fn dispatch_control_in(
        device: &nusb::Device,
        interfaces: &HashMap<u8, nusb::Interface>,
        control: ControlIn,
    ) -> Result<Vec<u8>, nusb::transfer::TransferError> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = interfaces;
            return device.control_in(control, TRANSFER_TIMEOUT).wait();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = device;
            let Some(interface) = interfaces.values().next() else {
                return Err(nusb::transfer::TransferError::InvalidArgument);
            };
            interface.control_in(control, TRANSFER_TIMEOUT).wait()
        }
    }

    fn dispatch_control_out(
        device: &nusb::Device,
        interfaces: &HashMap<u8, nusb::Interface>,
        control: ControlOut<'_>,
    ) -> Result<(), nusb::transfer::TransferError> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = interfaces;
            return device.control_out(control, TRANSFER_TIMEOUT).wait();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = device;
            let Some(interface) = interfaces.values().next() else {
                return Err(nusb::transfer::TransferError::InvalidArgument);
            };
            interface.control_out(control, TRANSFER_TIMEOUT).wait()
        }
    }

    fn map_in_result(
        result: Result<Vec<u8>, nusb::transfer::TransferError>,
    ) -> Result<InTransfer, AdapterError> {
        match result {
            Ok(data) => Ok(InTransfer {
                status: TransferStatus::Ok,
                data: Some(data),
            }),
            Err(err) => map_transfer_error(err),
        }
    }

    fn map_out_result(
        result: Result<(), nusb::transfer::TransferError>,
        len: usize,
    ) -> Result<OutTransfer, AdapterError> {
        match result {
            Ok(()) => Ok(OutTransfer {
                status: TransferStatus::Ok,
                bytes_written: len,
            }),
            Err(nusb::transfer::TransferError::Stall) => Ok(OutTransfer {
                status: TransferStatus::Stall,
                bytes_written: 0,
            }),
            Err(nusb::transfer::TransferError::Fault) => Ok(OutTransfer {
                status: TransferStatus::Babble,
                bytes_written: 0,
            }),
            Err(err) => Err(AdapterError::new("NetworkError", err.to_string())),
        }
    }

    fn map_transfer_error(err: nusb::transfer::TransferError) -> Result<InTransfer, AdapterError> {
        match err {
            nusb::transfer::TransferError::Stall => Ok(InTransfer {
                status: TransferStatus::Stall,
                data: None,
            }),
            nusb::transfer::TransferError::Fault => Ok(InTransfer {
                status: TransferStatus::Babble,
                data: None,
            }),
            other => Err(AdapterError::new("NetworkError", other.to_string())),
        }
    }

    fn completion_in(
        status: Result<(), nusb::transfer::TransferError>,
        data: Vec<u8>,
    ) -> Result<InTransfer, AdapterError> {
        match status {
            Ok(()) => Ok(InTransfer {
                status: TransferStatus::Ok,
                data: Some(data),
            }),
            Err(err) => map_transfer_error(err),
        }
    }

    fn completion_out(
        status: Result<(), nusb::transfer::TransferError>,
        actual: usize,
        requested: usize,
    ) -> Result<OutTransfer, AdapterError> {
        match status {
            Ok(()) => Ok(OutTransfer {
                status: TransferStatus::Ok,
                bytes_written: actual.min(requested),
            }),
            Err(nusb::transfer::TransferError::Stall) => Ok(OutTransfer {
                status: TransferStatus::Stall,
                bytes_written: 0,
            }),
            Err(nusb::transfer::TransferError::Fault) => Ok(OutTransfer {
                status: TransferStatus::Babble,
                bytes_written: 0,
            }),
            Err(err) => Err(AdapterError::new("NetworkError", err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(class: u8) -> UsbInfo {
        UsbInfo {
            id: "dev-1".into(),
            vendor_id: 0x1111,
            product_id: 0x2222,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            device_version: 0x0100,
            usb_version: 0x0200,
            manufacturer: Some("w3cos".into()),
            product: Some("gadget".into()),
            serial: Some("SN".into()),
            interface_classes: vec![InterfaceClass {
                number: 0,
                class,
                subclass: 0,
                protocol: 0,
            }],
            configurations: vec![UsbConfigurationInfo {
                value: 1,
                name: None,
                interfaces: vec![UsbInterfaceInfo {
                    number: 0,
                    alternates: vec![UsbAlternateInfo {
                        setting: 0,
                        class,
                        subclass: 0,
                        protocol: 0,
                        name: None,
                        endpoints: vec![
                            UsbEndpointInfo {
                                address: 0x81,
                                packet_size: 64,
                                kind: UsbEndpointKind::Bulk,
                            },
                            UsbEndpointInfo {
                                address: 0x01,
                                packet_size: 64,
                                kind: UsbEndpointKind::Bulk,
                            },
                        ],
                    }],
                }],
            }],
            active_configuration: Some(1),
        }
    }

    #[test]
    fn filters_match_vendor_and_interface_class() {
        let info = sample(0xFF);
        assert!(info.matches(&UsbFilter {
            vendor_id: Some(0x1111),
            class_code: Some(0xFF),
            serial_number: Some("SN".into()),
            ..UsbFilter::default()
        }));
        assert!(!info.matches(&UsbFilter {
            product_id: Some(0x0001),
            ..UsbFilter::default()
        }));
        assert!(!info.matches(&UsbFilter {
            class_code: Some(0x03),
            ..UsbFilter::default()
        }));
    }

    #[test]
    fn protected_classes_block_hid_and_mass_storage_claims() {
        assert!(is_protected_interface(0x03));
        assert!(is_protected_interface(0x08));
        assert!(!is_protected_interface(0xFF));
        reset();
        set_mock_devices(vec![MockUsbSpec {
            info: sample(0x03),
            inputs: VecDeque::new(),
            output_status: VecDeque::new(),
        }]);
        open("dev-1").unwrap();
        let error = claim_interface("dev-1", 0).expect_err("protected");
        assert_eq!(error.name, "SecurityError");
        reset();
    }

    #[test]
    fn scripted_transfers_map_stall_and_byte_counts() {
        reset();
        set_mock_devices(vec![MockUsbSpec {
            info: sample(0xFF),
            inputs: VecDeque::from([MockIn::Data(vec![1, 2, 3]), MockIn::Stall]),
            output_status: VecDeque::from([TransferStatus::Ok, TransferStatus::Stall]),
        }]);
        open("dev-1").unwrap();
        claim_interface("dev-1", 0).unwrap();
        let incoming = transfer_in("dev-1", 1, 64).unwrap();
        assert_eq!(incoming.status, TransferStatus::Ok);
        assert_eq!(incoming.data.unwrap(), vec![1, 2, 3]);
        let stalled = transfer_in("dev-1", 1, 64).unwrap();
        assert_eq!(stalled.status, TransferStatus::Stall);
        assert!(stalled.data.is_none());
        let outgoing = transfer_out("dev-1", 1, &[9, 8]).unwrap();
        assert_eq!(outgoing.bytes_written, 2);
        assert_eq!(outgoing.status, TransferStatus::Ok);
        let stalled_out = transfer_out("dev-1", 1, &[7]).unwrap();
        assert_eq!(stalled_out.status, TransferStatus::Stall);
        assert_eq!(stalled_out.bytes_written, 0);
        assert_eq!(output_log("dev-1"), vec![vec![9, 8], vec![7]]);
        reset();
    }

    #[test]
    fn usb_hardware_enumeration_can_open_and_close() {
        if std::env::var_os("W3COS_USB_DEVICE_TESTS").is_none() {
            eprintln!("skipping USB hardware test: set W3COS_USB_DEVICE_TESTS=1");
            return;
        }
        let devices = platform_list().expect("nusb enumeration");
        eprintln!("USB devices: {}", devices.len());
        if let Some(device) = devices.first() {
            let _ = open(&device.id).expect("open first USB device");
            close(&device.id);
        }
    }
}
