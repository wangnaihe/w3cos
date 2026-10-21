//! WebHID platform adapter.
//!
//! Desktop builds with `web-device-access` talk to real devices through
//! `hidapi` (Linux: hidraw via `linux-native-basic-udev`, so the runtime does
//! not link libusb or libudev). Android, iOS, and OHOS have no adapter.
//!
//! Unit tests install a mock registry. Hardware enumeration is opt-in via
//! `W3COS_HID_DEVICE_TESTS=1` and is never touched by the default test path.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    static MOCK: RefCell<Option<MockRegistry>> = const { RefCell::new(None) };
    static CHOOSER: RefCell<Option<Chooser>> = const { RefCell::new(None) };
    static OPEN: RefCell<HashMap<String, HidSession>> = RefCell::new(HashMap::new());
    static SEEN: RefCell<Option<HashSet<String>>> = const { RefCell::new(None) };
    static LAST_HOTPLUG: RefCell<Option<Instant>> = const { RefCell::new(None) };
    static LIST_WARNING: RefCell<bool> = const { RefCell::new(false) };
}

type Chooser = Rc<dyn Fn(&[HidInfo]) -> Option<String>>;

pub enum HidChoice {
    NoChooser,
    Dismissed,
    Selected(HidInfo),
}

#[derive(Clone, Debug)]
pub struct HidReportItem {
    pub is_absolute: bool,
    pub is_array: bool,
    pub is_range: bool,
    pub report_size: u32,
    pub report_count: u32,
    pub usages: Vec<u32>,
    pub usage_minimum: u32,
    pub usage_maximum: u32,
}

#[derive(Clone, Debug)]
pub struct HidReport {
    pub report_id: u8,
    pub items: Vec<HidReportItem>,
}

#[derive(Clone, Debug)]
pub struct HidCollection {
    pub usage_page: u16,
    pub usage: u16,
    pub collection_type: u8,
    pub children: Vec<HidCollection>,
    pub input_reports: Vec<HidReport>,
    pub output_reports: Vec<HidReport>,
    pub feature_reports: Vec<HidReport>,
}

#[derive(Clone, Debug)]
pub struct HidInfo {
    pub id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_name: String,
    #[allow(dead_code)]
    pub serial_number: String,
    pub collections: Vec<HidCollection>,
    pub uses_report_ids: bool,
}

#[derive(Clone, Debug, Default)]
pub struct HidFilter {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub usage_page: Option<u16>,
    pub usage: Option<u16>,
}

#[derive(Debug)]
pub struct AdapterError {
    pub name: &'static str,
    pub message: String,
}

impl AdapterError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParsedDescriptor {
    pub collections: Vec<HidCollection>,
    pub uses_report_ids: bool,
}

#[derive(Clone, Debug)]
pub struct MockHidSpec {
    pub id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_name: String,
    pub serial_number: String,
    pub usage_page: u16,
    pub usage: u16,
    pub descriptor: Vec<u8>,
}

pub enum HotplugEvent {
    Connected(HidInfo),
    Disconnected(String),
}

struct MockEndpoint {
    incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
    output: Arc<Mutex<Vec<Vec<u8>>>>,
    features: Arc<Mutex<HashMap<u8, Vec<u8>>>>,
}

struct MockRegistry {
    devices: Vec<HidInfo>,
    endpoints: HashMap<String, MockEndpoint>,
}

enum HidCmd {
    Output(Vec<u8>, Sender<Result<(), String>>),
    FeatureOut(Vec<u8>, Sender<Result<(), String>>),
    FeatureIn(u8, usize, Sender<Result<Vec<u8>, String>>),
    Close,
}

struct HidSession {
    incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
    lost: Arc<AtomicBool>,
    kind: SessionKind,
}

enum SessionKind {
    Mock {
        output: Arc<Mutex<Vec<Vec<u8>>>>,
        features: Arc<Mutex<HashMap<u8, Vec<u8>>>>,
    },
    #[cfg(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    Native {
        tx: Sender<HidCmd>,
        join: Option<JoinHandle<()>>,
    },
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
    let sessions: Vec<HidSession> =
        OPEN.with(|open| open.borrow_mut().drain().map(|(_, s)| s).collect());
    for session in sessions {
        session.shutdown();
    }
    MOCK.with(|mock| *mock.borrow_mut() = None);
    CHOOSER.with(|chooser| *chooser.borrow_mut() = None);
    SEEN.with(|seen| *seen.borrow_mut() = None);
    LAST_HOTPLUG.with(|last| *last.borrow_mut() = None);
    LIST_WARNING.with(|warned| *warned.borrow_mut() = false);
}

pub fn set_chooser(hook: Option<Box<dyn Fn(&[HidInfo]) -> Option<String>>>) {
    CHOOSER.with(|chooser| {
        *chooser.borrow_mut() = hook.map(|hook| Rc::from(hook) as Chooser);
    });
}

pub fn choose(devices: &[HidInfo]) -> Result<HidChoice, AdapterError> {
    let hook = CHOOSER.with(|chooser| chooser.borrow().clone());
    let Some(hook) = hook else {
        return Ok(HidChoice::NoChooser);
    };
    match hook(devices) {
        Some(id) => devices
            .iter()
            .find(|device| device.id == id)
            .cloned()
            .map(HidChoice::Selected)
            .ok_or_else(|| {
                AdapterError::new(
                    "NotFoundError",
                    "the HID chooser selected a device that is not connected",
                )
            }),
        None => Ok(HidChoice::Dismissed),
    }
}

#[allow(dead_code)]
pub fn chooser_installed() -> bool {
    CHOOSER.with(|chooser| chooser.borrow().is_some())
}

pub fn using_mock() -> bool {
    MOCK.with(|mock| mock.borrow().is_some())
}

pub fn any_open() -> bool {
    OPEN.with(|open| !open.borrow().is_empty())
}

pub fn set_mock_devices(specs: Vec<MockHidSpec>) {
    let mut devices = Vec::new();
    let mut endpoints = HashMap::new();
    for spec in specs {
        let info = info_from_spec(spec);
        endpoints.insert(
            info.id.clone(),
            MockEndpoint {
                incoming: Arc::new(Mutex::new(VecDeque::new())),
                output: Arc::new(Mutex::new(Vec::new())),
                features: Arc::new(Mutex::new(HashMap::new())),
            },
        );
        devices.push(info);
    }
    MOCK.with(|mock| {
        *mock.borrow_mut() = Some(MockRegistry { devices, endpoints });
    });
}

pub fn push_input(id: &str, report: Vec<u8>) -> bool {
    MOCK.with(|mock| {
        let mock = mock.borrow();
        let Some(registry) = mock.as_ref() else {
            return false;
        };
        let Some(endpoint) = registry.endpoints.get(id) else {
            return false;
        };
        endpoint
            .incoming
            .lock()
            .expect("hid mock input")
            .push_back(report);
        true
    })
}

pub fn output_log(id: &str) -> Vec<Vec<u8>> {
    MOCK.with(|mock| {
        mock.borrow()
            .as_ref()
            .and_then(|registry| registry.endpoints.get(id))
            .map(|endpoint| endpoint.output.lock().expect("hid mock output").clone())
            .unwrap_or_default()
    })
}

pub fn set_feature(id: &str, report_id: u8, data: Vec<u8>) {
    MOCK.with(|mock| {
        if let Some(endpoint) = mock
            .borrow()
            .as_ref()
            .and_then(|registry| registry.endpoints.get(id))
        {
            endpoint
                .features
                .lock()
                .expect("hid mock feature")
                .insert(report_id, data);
        }
    });
}

#[allow(dead_code)]
pub fn feature_log(id: &str) -> HashMap<u8, Vec<u8>> {
    MOCK.with(|mock| {
        mock.borrow()
            .as_ref()
            .and_then(|registry| registry.endpoints.get(id))
            .map(|endpoint| endpoint.features.lock().expect("hid mock feature").clone())
            .unwrap_or_default()
    })
}

fn info_from_spec(spec: MockHidSpec) -> HidInfo {
    let parsed = parse_report_descriptor(&spec.descriptor);
    let collections = if parsed.collections.is_empty() {
        vec![stub_collection(spec.usage_page, spec.usage)]
    } else {
        parsed.collections
    };
    HidInfo {
        id: spec.id,
        vendor_id: spec.vendor_id,
        product_id: spec.product_id,
        product_name: spec.product_name,
        serial_number: spec.serial_number,
        uses_report_ids: parsed.uses_report_ids,
        collections,
    }
}

pub fn list_devices() -> Vec<HidInfo> {
    if let Some(devices) = MOCK.with(|mock| {
        mock.borrow()
            .as_ref()
            .map(|registry| registry.devices.clone())
    }) {
        return devices;
    }
    if cfg!(test) && std::env::var_os("W3COS_HID_DEVICE_TESTS").is_none() {
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
                eprintln!("[w3cos] warning: WebHID enumeration failed: {err}");
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
        Ok(devices) => format!("hidapi {} device(s)", devices.len()),
        Err(err) => format!("hidapi error: {err}"),
    }
}

pub fn open(info: &HidInfo) -> Result<(), AdapterError> {
    if OPEN.with(|open| open.borrow().contains_key(&info.id)) {
        return Ok(());
    }
    let session = if MOCK.with(|mock| mock.borrow().is_some()) {
        open_mock(info)?
    } else {
        open_platform(info)?
    };
    OPEN.with(|open| {
        open.borrow_mut().insert(info.id.clone(), session);
    });
    Ok(())
}

pub fn close(id: &str) {
    if let Some(session) = OPEN.with(|open| open.borrow_mut().remove(id)) {
        session.shutdown();
    }
}

pub fn is_open(id: &str) -> bool {
    OPEN.with(|open| open.borrow().contains_key(id))
}

pub fn send_output(id: &str, report_id: u8, data: &[u8]) -> Result<(), AdapterError> {
    let payload = report_bytes(report_id, data);
    with_session(id, |session| session.write_output(&payload))
}

pub fn send_feature(id: &str, report_id: u8, data: &[u8]) -> Result<(), AdapterError> {
    let payload = report_bytes(report_id, data);
    with_session(id, |session| session.write_feature(&payload))
}

pub fn receive_feature(
    id: &str,
    report_id: u8,
    payload_len: usize,
) -> Result<Vec<u8>, AdapterError> {
    with_session(id, |session| session.read_feature(report_id, payload_len))
}

pub struct InputBatch {
    pub id: String,
    pub reports: Vec<Vec<u8>>,
    pub lost: bool,
}

pub fn poll_inputs() -> Vec<InputBatch> {
    OPEN.with(|open| {
        let mut lost = Vec::new();
        let mut batches = Vec::new();
        for (id, session) in open.borrow().iter() {
            let reports = session
                .incoming
                .lock()
                .expect("hid input queue")
                .drain(..)
                .collect::<Vec<_>>();
            let failed = session.lost.load(Ordering::SeqCst);
            if !reports.is_empty() || failed {
                batches.push(InputBatch {
                    id: id.clone(),
                    reports,
                    lost: failed,
                });
            }
            if failed {
                lost.push(id.clone());
            }
        }
        for id in lost {
            if let Some(session) = open.borrow_mut().remove(&id) {
                session.shutdown();
            }
        }
        batches
    })
}

pub fn poll_hotplug(allowed: &HashSet<String>) -> Vec<HotplugEvent> {
    let throttle = !cfg!(test);
    if throttle {
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
            events.push(HotplugEvent::Disconnected(id.clone()));
        }
    }
    SEEN.with(|seen| *seen.borrow_mut() = Some(ids));
    events
}

impl HidInfo {
    pub fn matches(&self, filter: &HidFilter) -> bool {
        if filter.vendor_id.is_some_and(|id| id != self.vendor_id) {
            return false;
        }
        if filter.product_id.is_some_and(|id| id != self.product_id) {
            return false;
        }
        match (filter.usage_page, filter.usage) {
            (None, None) => true,
            (Some(page), None) => self
                .collections
                .iter()
                .any(|collection| collection.usage_page == page),
            (None, Some(usage)) => self
                .collections
                .iter()
                .any(|collection| collection.usage == usage),
            (Some(page), Some(usage)) => self
                .collections
                .iter()
                .any(|collection| collection.usage_page == page && collection.usage == usage),
        }
    }

    pub fn feature_payload_len(&self, report_id: u8) -> usize {
        fn walk(collections: &[HidCollection], report_id: u8) -> Option<usize> {
            for collection in collections {
                for report in &collection.feature_reports {
                    if report.report_id == report_id {
                        let bits: u32 = report
                            .items
                            .iter()
                            .map(|item| item.report_size.saturating_mul(item.report_count))
                            .sum();
                        return Some((bits as usize).div_ceil(8).max(1));
                    }
                }
                if let Some(len) = walk(&collection.children, report_id) {
                    return Some(len);
                }
            }
            None
        }
        walk(&self.collections, report_id).unwrap_or(64)
    }
}

pub fn split_input_report(uses_report_ids: bool, raw: &[u8]) -> (u8, Vec<u8>) {
    if uses_report_ids {
        match raw.split_first() {
            Some((id, rest)) => (*id, rest.to_vec()),
            None => (0, Vec::new()),
        }
    } else {
        (0, raw.to_vec())
    }
}

fn report_bytes(report_id: u8, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(data.len() + 1);
    payload.push(report_id);
    payload.extend_from_slice(data);
    payload
}

fn with_session<T>(
    id: &str,
    body: impl FnOnce(&HidSession) -> Result<T, AdapterError>,
) -> Result<T, AdapterError> {
    OPEN.with(|open| {
        let open = open.borrow();
        let Some(session) = open.get(id) else {
            return Err(AdapterError::new(
                "InvalidStateError",
                "HIDDevice is not open",
            ));
        };
        body(session)
    })
}

fn open_mock(info: &HidInfo) -> Result<HidSession, AdapterError> {
    let endpoint = MOCK.with(|mock| {
        mock.borrow().as_ref().and_then(|registry| {
            registry
                .endpoints
                .get(&info.id)
                .map(|endpoint| MockEndpoint {
                    incoming: Arc::clone(&endpoint.incoming),
                    output: Arc::clone(&endpoint.output),
                    features: Arc::clone(&endpoint.features),
                })
        })
    });
    let Some(endpoint) = endpoint else {
        return Err(AdapterError::new(
            "NotFoundError",
            "HID device is no longer connected",
        ));
    };
    Ok(HidSession {
        incoming: endpoint.incoming,
        lost: Arc::new(AtomicBool::new(false)),
        kind: SessionKind::Mock {
            output: endpoint.output,
            features: endpoint.features,
        },
    })
}

fn open_platform(info: &HidInfo) -> Result<HidSession, AdapterError> {
    #[cfg(all(
        feature = "web-device-access",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    {
        return native::open(info);
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
        let _ = info;
        Err(AdapterError::new(
            "NotFoundError",
            "this build has no WebHID adapter",
        ))
    }
}

fn platform_list() -> Result<Vec<HidInfo>, String> {
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
        Err("this build has no WebHID adapter".into())
    }
}

impl HidSession {
    fn write_output(&self, data: &[u8]) -> Result<(), AdapterError> {
        if self.lost.load(Ordering::SeqCst) {
            return Err(AdapterError::new("NetworkError", "HID device disconnected"));
        }
        match &self.kind {
            SessionKind::Mock { output, .. } => {
                output.lock().expect("hid mock output").push(data.to_vec());
                Ok(())
            }
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { tx, .. } => {
                let (reply_tx, reply_rx) = mpsc::channel();
                tx.send(HidCmd::Output(data.to_vec(), reply_tx))
                    .map_err(|_| AdapterError::new("NetworkError", "HID device disconnected"))?;
                wait_reply(reply_rx)
            }
        }
    }

    fn write_feature(&self, data: &[u8]) -> Result<(), AdapterError> {
        match &self.kind {
            SessionKind::Mock { features, .. } => {
                let id = data.first().copied().unwrap_or(0);
                let payload = if data.is_empty() {
                    Vec::new()
                } else {
                    data[1..].to_vec()
                };
                features
                    .lock()
                    .expect("hid mock feature")
                    .insert(id, payload);
                Ok(())
            }
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { tx, .. } => {
                let (reply_tx, reply_rx) = mpsc::channel();
                tx.send(HidCmd::FeatureOut(data.to_vec(), reply_tx))
                    .map_err(|_| AdapterError::new("NetworkError", "HID device disconnected"))?;
                wait_reply(reply_rx)
            }
        }
    }

    fn read_feature(&self, report_id: u8, payload_len: usize) -> Result<Vec<u8>, AdapterError> {
        match &self.kind {
            SessionKind::Mock { features, .. } => features
                .lock()
                .expect("hid mock feature")
                .get(&report_id)
                .cloned()
                .ok_or_else(|| {
                    AdapterError::new("NotFoundError", "the feature report is not available")
                }),
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { tx, .. } => {
                let (reply_tx, reply_rx) = mpsc::channel();
                tx.send(HidCmd::FeatureIn(
                    report_id,
                    payload_len.max(1) + 1,
                    reply_tx,
                ))
                .map_err(|_| AdapterError::new("NetworkError", "HID device disconnected"))?;
                match reply_rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(Ok(bytes)) => Ok(bytes),
                    Ok(Err(err)) => Err(AdapterError::new("NetworkError", err)),
                    Err(RecvTimeoutError::Timeout) => Err(AdapterError::new(
                        "NetworkError",
                        "HID feature report timed out",
                    )),
                    Err(RecvTimeoutError::Disconnected) => {
                        Err(AdapterError::new("NetworkError", "HID device disconnected"))
                    }
                }
            }
        }
    }

    fn shutdown(self) {
        match self.kind {
            SessionKind::Mock { .. } => {}
            #[cfg(all(
                feature = "web-device-access",
                any(
                    target_os = "macos",
                    target_os = "windows",
                    all(target_os = "linux", not(target_env = "ohos"))
                )
            ))]
            SessionKind::Native { tx, join } => {
                let _ = tx.send(HidCmd::Close);
                if let Some(join) = join {
                    let _ = join.join();
                }
            }
        }
    }
}

fn wait_reply(reply_rx: Receiver<Result<(), String>>) -> Result<(), AdapterError> {
    match reply_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(AdapterError::new("NetworkError", err)),
        Err(RecvTimeoutError::Timeout) => {
            Err(AdapterError::new("NetworkError", "HID request timed out"))
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(AdapterError::new("NetworkError", "HID device disconnected"))
        }
    }
}

fn collections_have_reports(collections: &[HidCollection]) -> bool {
    collections.iter().any(|collection| {
        !collection.input_reports.is_empty()
            || !collection.output_reports.is_empty()
            || !collection.feature_reports.is_empty()
            || collections_have_reports(&collection.children)
    })
}

fn stub_collection(usage_page: u16, usage: u16) -> HidCollection {
    HidCollection {
        usage_page,
        usage,
        collection_type: 1,
        children: Vec::new(),
        input_reports: Vec::new(),
        output_reports: Vec::new(),
        feature_reports: Vec::new(),
    }
}

#[derive(Clone)]
struct GlobalState {
    usage_page: u16,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

struct LocalState {
    usages: Vec<u32>,
    usage_min: Option<u32>,
    usage_max: Option<u32>,
}

struct CollectionBuild {
    usage_page: u16,
    usage: u16,
    collection_type: u8,
    children: Vec<HidCollection>,
    input_reports: Vec<HidReport>,
    output_reports: Vec<HidReport>,
    feature_reports: Vec<HidReport>,
}

pub fn parse_report_descriptor(bytes: &[u8]) -> ParsedDescriptor {
    let mut global = GlobalState {
        usage_page: 0,
        report_size: 0,
        report_count: 0,
        report_id: 0,
    };
    let mut global_stack = Vec::new();
    let mut local = LocalState {
        usages: Vec::new(),
        usage_min: None,
        usage_max: None,
    };
    let mut stack = vec![CollectionBuild {
        usage_page: 0,
        usage: 0,
        collection_type: 0,
        children: Vec::new(),
        input_reports: Vec::new(),
        output_reports: Vec::new(),
        feature_reports: Vec::new(),
    }];
    let mut uses_report_ids = false;
    let mut index = 0;
    while index < bytes.len() {
        let header = bytes[index];
        index += 1;
        if header == 0xFE {
            if index >= bytes.len() {
                break;
            }
            let size = bytes[index] as usize;
            index += 2;
            index = index.saturating_add(size).min(bytes.len());
            continue;
        }
        let mut size = (header & 0x03) as usize;
        if size == 3 {
            size = 4;
        }
        let item_type = (header >> 2) & 0x03;
        let tag = header >> 4;
        if index + size > bytes.len() {
            break;
        }
        let raw = item_u32(&bytes[index..index + size]);
        index += size;
        match item_type {
            1 => match tag {
                0 => global.usage_page = raw as u16,
                7 => global.report_size = raw,
                8 => {
                    global.report_id = raw as u8;
                    uses_report_ids = true;
                }
                9 => global.report_count = raw,
                10 => global_stack.push(global.clone()),
                11 => {
                    if let Some(restored) = global_stack.pop() {
                        global = restored;
                    }
                }
                _ => {}
            },
            2 => match tag {
                0 => local.usages.push(raw),
                1 => local.usage_min = Some(raw),
                2 => local.usage_max = Some(raw),
                _ => {}
            },
            0 => match tag {
                8 | 9 | 11 => {
                    let item = report_item(&global, &local, raw);
                    let reports = match tag {
                        8 => &mut stack.last_mut().expect("collection").input_reports,
                        9 => &mut stack.last_mut().expect("collection").output_reports,
                        _ => &mut stack.last_mut().expect("collection").feature_reports,
                    };
                    push_report(reports, global.report_id, item);
                    local = LocalState {
                        usages: Vec::new(),
                        usage_min: None,
                        usage_max: None,
                    };
                }
                10 => {
                    let (usage_page, usage) = local
                        .usages
                        .first()
                        .map(|usage| split_usage(global.usage_page, *usage, 4))
                        .unwrap_or((global.usage_page, 0));
                    stack.push(CollectionBuild {
                        usage_page,
                        usage,
                        collection_type: raw as u8,
                        children: Vec::new(),
                        input_reports: Vec::new(),
                        output_reports: Vec::new(),
                        feature_reports: Vec::new(),
                    });
                    local = LocalState {
                        usages: Vec::new(),
                        usage_min: None,
                        usage_max: None,
                    };
                }
                12 => {
                    if stack.len() > 1 {
                        let done = stack.pop().expect("collection");
                        stack
                            .last_mut()
                            .expect("parent")
                            .children
                            .push(finish_collection(done));
                    }
                    local = LocalState {
                        usages: Vec::new(),
                        usage_min: None,
                        usage_max: None,
                    };
                }
                _ => {}
            },
            _ => {}
        }
    }
    while stack.len() > 1 {
        let done = stack.pop().expect("collection");
        stack
            .last_mut()
            .expect("parent")
            .children
            .push(finish_collection(done));
    }
    ParsedDescriptor {
        collections: stack.pop().map(|root| root.children).unwrap_or_default(),
        uses_report_ids,
    }
}

fn finish_collection(collection: CollectionBuild) -> HidCollection {
    HidCollection {
        usage_page: collection.usage_page,
        usage: collection.usage,
        collection_type: collection.collection_type,
        children: collection.children,
        input_reports: collection.input_reports,
        output_reports: collection.output_reports,
        feature_reports: collection.feature_reports,
    }
}

fn push_report(reports: &mut Vec<HidReport>, report_id: u8, item: HidReportItem) {
    if let Some(report) = reports
        .iter_mut()
        .find(|report| report.report_id == report_id)
    {
        report.items.push(item);
    } else {
        reports.push(HidReport {
            report_id,
            items: vec![item],
        });
    }
}

fn report_item(global: &GlobalState, local: &LocalState, flags: u32) -> HidReportItem {
    let is_range = local.usages.is_empty() && local.usage_min.is_some();
    let usages = local
        .usages
        .iter()
        .map(|usage| split_usage(global.usage_page, *usage, 4).1 as u32)
        .collect();
    HidReportItem {
        // HID main-item flags: bit 1 set means Variable, bit 2 set means Relative.
        is_absolute: flags & 0x04 == 0,
        is_array: flags & 0x02 == 0,
        is_range,
        report_size: global.report_size,
        report_count: global.report_count,
        usages,
        usage_minimum: local.usage_min.unwrap_or(0),
        usage_maximum: local.usage_max.unwrap_or(0),
    }
}

fn split_usage(page: u16, raw: u32, _size: usize) -> (u16, u16) {
    if raw > 0xFFFF {
        ((raw >> 16) as u16, (raw & 0xFFFF) as u16)
    } else {
        (page, raw as u16)
    }
}

fn item_u32(bytes: &[u8]) -> u32 {
    let mut value = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        value |= u32::from(*byte) << (8 * index);
    }
    value
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
    use std::ffi::CString;

    pub fn list() -> Result<Vec<HidInfo>, String> {
        let api = hidapi::HidApi::new().map_err(|err| err.to_string())?;
        let mut order = Vec::new();
        let mut grouped: HashMap<String, HidInfo> = HashMap::new();
        for device in api.device_list() {
            let path = device.path().to_string_lossy().into_owned();
            let id = if path.is_empty() {
                format!(
                    "{:04x}:{:04x}:{:04x}:{:04x}",
                    device.vendor_id(),
                    device.product_id(),
                    device.usage_page(),
                    device.usage()
                )
            } else {
                path.clone()
            };
            if let Some(existing) = grouped.get_mut(&id) {
                let parsed = collections_have_reports(&existing.collections);
                let known = existing.collections.iter().any(|collection| {
                    collection.usage_page == device.usage_page()
                        && collection.usage == device.usage()
                });
                if !parsed && !known {
                    existing
                        .collections
                        .push(stub_collection(device.usage_page(), device.usage()));
                }
                continue;
            }
            let descriptor = read_descriptor(&path);
            let parsed = parse_report_descriptor(&descriptor);
            let from_descriptor = !parsed.collections.is_empty();
            let collections = if from_descriptor {
                parsed.collections
            } else {
                vec![stub_collection(device.usage_page(), device.usage())]
            };
            order.push(id.clone());
            grouped.insert(
                id.clone(),
                HidInfo {
                    id,
                    vendor_id: device.vendor_id(),
                    product_id: device.product_id(),
                    product_name: device.product_string().unwrap_or("").to_string(),
                    serial_number: device.serial_number().unwrap_or("").to_string(),
                    collections,
                    uses_report_ids: parsed.uses_report_ids,
                },
            );
        }
        Ok(order
            .into_iter()
            .filter_map(|id| grouped.remove(&id))
            .collect())
    }

    pub fn open(info: &HidInfo) -> Result<HidSession, AdapterError> {
        let api = hidapi::HidApi::new()
            .map_err(|err| AdapterError::new("NetworkError", err.to_string()))?;
        let path = CString::new(info.id.clone()).map_err(|_| {
            AdapterError::new("NotFoundError", "HID device path is not a valid string")
        })?;
        let device = api.open_path(&path).map_err(|err| {
            AdapterError::new("NotFoundError", format!("failed to open HID device: {err}"))
        })?;
        let (tx, rx) = mpsc::channel();
        let incoming = Arc::new(Mutex::new(VecDeque::new()));
        let lost = Arc::new(AtomicBool::new(false));
        let incoming_for_thread = Arc::clone(&incoming);
        let lost_for_thread = Arc::clone(&lost);
        let join = std::thread::Builder::new()
            .name("w3cos-hid".into())
            .spawn(move || reader(device, rx, incoming_for_thread, lost_for_thread))
            .map_err(|err| AdapterError::new("NetworkError", err.to_string()))?;
        Ok(HidSession {
            incoming,
            lost,
            kind: SessionKind::Native {
                tx,
                join: Some(join),
            },
        })
    }

    fn reader(
        device: hidapi::HidDevice,
        rx: Receiver<HidCmd>,
        incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
        lost: Arc<AtomicBool>,
    ) {
        let mut buf = [0u8; 2048];
        loop {
            let mut closing = false;
            loop {
                match rx.try_recv() {
                    Ok(HidCmd::Close) | Err(TryRecvError::Disconnected) => {
                        closing = true;
                        break;
                    }
                    Ok(HidCmd::Output(data, reply)) => {
                        let _ = reply.send(
                            device
                                .write(&data)
                                .map(|_| ())
                                .map_err(|err| err.to_string()),
                        );
                    }
                    Ok(HidCmd::FeatureOut(data, reply)) => {
                        let _ = reply.send(
                            device
                                .send_feature_report(&data)
                                .map_err(|err| err.to_string()),
                        );
                    }
                    Ok(HidCmd::FeatureIn(id, len, reply)) => {
                        let mut data = vec![0u8; len.max(2)];
                        data[0] = id;
                        let result = device.get_feature_report(&mut data).map(|count| {
                            let count = count.min(data.len());
                            let payload = data[..count].to_vec();
                            if payload.is_empty() {
                                Vec::new()
                            } else {
                                payload[1..].to_vec()
                            }
                        });
                        let _ = reply.send(result.map_err(|err| err.to_string()));
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if closing {
                break;
            }
            match device.read_timeout(&mut buf, 20) {
                Ok(0) => {}
                Ok(count) => {
                    if let Ok(mut queue) = incoming.lock() {
                        queue.push_back(buf[..count].to_vec());
                    }
                }
                Err(err) => {
                    let text = err.to_string().to_ascii_lowercase();
                    if text.contains("timeout") || text.contains("timed out") {
                        continue;
                    }
                    lost.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    }

    fn read_descriptor(path: &str) -> Vec<u8> {
        #[cfg(target_os = "linux")]
        {
            let Some(name) = std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
            else {
                return Vec::new();
            };
            return std::fs::read(format!("/sys/class/hidraw/{name}/device/report_descriptor"))
                .unwrap_or_default();
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse_descriptor() -> Vec<u8> {
        vec![
            0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00,
            0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02,
            0x81, 0x06, 0xC0,
        ]
    }

    #[test]
    fn report_descriptor_exposes_top_level_mouse_collection() {
        let parsed = parse_report_descriptor(&mouse_descriptor());
        assert!(!parsed.uses_report_ids);
        assert_eq!(parsed.collections.len(), 1);
        let collection = &parsed.collections[0];
        assert_eq!(collection.usage_page, 0x01);
        assert_eq!(collection.usage, 0x02);
        assert_eq!(collection.input_reports.len(), 1);
        let items = &collection.input_reports[0].items;
        assert!(items[0].is_range);
        assert!(items[0].is_absolute);
        assert!(!items[0].is_array);
        assert_eq!(items[0].usage_minimum, 1);
        assert_eq!(items[0].usage_maximum, 3);
        assert_eq!(items[0].report_size, 1);
        assert_eq!(items[0].report_count, 3);
        let axes = items
            .iter()
            .find(|item| item.usages == vec![0x30, 0x31])
            .expect("axes");
        assert!(!axes.is_absolute);
        assert!(!axes.is_array);
    }

    #[test]
    fn filters_match_vendor_and_the_same_top_level_usage() {
        let info = info_from_spec(MockHidSpec {
            id: "mouse".into(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            product_name: "Mouse".into(),
            serial_number: String::new(),
            usage_page: 0,
            usage: 0,
            descriptor: mouse_descriptor(),
        });
        assert!(info.matches(&HidFilter {
            vendor_id: Some(0x1234),
            product_id: None,
            usage_page: Some(0x01),
            usage: Some(0x02),
        }));
        assert!(!info.matches(&HidFilter {
            vendor_id: Some(0x1234),
            product_id: None,
            usage_page: Some(0x01),
            usage: Some(0x06),
        }));
        assert!(!info.matches(&HidFilter {
            vendor_id: Some(0x9999),
            product_id: None,
            usage_page: None,
            usage: None,
        }));
    }

    #[test]
    fn input_reports_keep_the_report_id_byte_only_when_the_descriptor_uses_ids() {
        assert_eq!(
            split_input_report(true, &[0x04, 0xAA, 0xBB]),
            (4, vec![0xAA, 0xBB])
        );
        assert_eq!(
            split_input_report(false, &[0xAA, 0xBB]),
            (0, vec![0xAA, 0xBB])
        );
    }

    #[test]
    fn hid_hardware_enumeration_can_open_and_close() {
        if std::env::var_os("W3COS_HID_DEVICE_TESTS").is_none() {
            eprintln!("skipping HID hardware test: set W3COS_HID_DEVICE_TESTS=1");
            return;
        }
        let devices = platform_list().expect("hidapi enumeration");
        eprintln!("HID devices: {}", devices.len());
        if let Some(device) = devices.first() {
            open(device).expect("open first HID device");
            close(&device.id);
        }
    }
}
