//! Desktop BLE radio for `navigator.bluetooth`.
//!
//! Scanning and GATT I/O run on one dedicated thread with a current-thread
//! tokio runtime. `btleplug` selects CoreBluetooth, WinRT, or BlueZ over
//! D-Bus. Replies block the caller; notifications and disconnects are queued
//! for the event loop.
//!
//! macOS CoreBluetooth refuses the radio unless the app bundle's Info.plist
//! contains `NSBluetoothAlwaysUsageDescription`. A missing key, a powered-off
//! adapter, or an absent BlueZ daemon makes `getAvailability` return false.
//! This module never inserts a peripheral the platform did not report.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use btleplug::api::{
    Central, CentralEvent, CentralState, CharPropFlags, Manager as _, Peripheral as _, ScanFilter,
    WriteType,
};
use btleplug::platform::{Manager, Peripheral, PeripheralId};
use futures::StreamExt;
use uuid::Uuid;

use super::{
    Advertisement, BluetoothFailure, CharProps, Filter, GattCharacteristic, GattDescriptor,
    GattService,
};

const SCAN_LIMIT: Duration = Duration::from_secs(5);
const SCAN_EARLY: Duration = Duration::from_millis(600);
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(8);

struct Shared {
    adapter: Option<btleplug::platform::Adapter>,
    devices: HashMap<String, Peripheral>,
    platform_ids: HashMap<PeripheralId, String>,
    watched: HashSet<String>,
    suppress: HashSet<String>,
    events_started: bool,
}

enum Command {
    Available(std::sync::mpsc::Sender<Result<bool, BluetoothFailure>>),
    Scan {
        filters: Vec<Filter>,
        accept_all: bool,
        complete: bool,
        reply: std::sync::mpsc::Sender<Result<Vec<Advertisement>, BluetoothFailure>>,
    },
    Connect(
        String,
        std::sync::mpsc::Sender<Result<(), BluetoothFailure>>,
    ),
    Disconnect(
        String,
        std::sync::mpsc::Sender<Result<(), BluetoothFailure>>,
    ),
    Services(
        String,
        std::sync::mpsc::Sender<Result<Vec<GattService>, BluetoothFailure>>,
    ),
    Read(
        String,
        String,
        String,
        std::sync::mpsc::Sender<Result<Vec<u8>, BluetoothFailure>>,
    ),
    Write(
        String,
        String,
        String,
        Vec<u8>,
        bool,
        std::sync::mpsc::Sender<Result<(), BluetoothFailure>>,
    ),
    Notify(
        String,
        String,
        String,
        bool,
        std::sync::mpsc::Sender<Result<(), BluetoothFailure>>,
    ),
    ReadDescriptor(
        String,
        String,
        String,
        String,
        std::sync::mpsc::Sender<Result<Vec<u8>, BluetoothFailure>>,
    ),
    WriteDescriptor(
        String,
        String,
        String,
        String,
        Vec<u8>,
        std::sync::mpsc::Sender<Result<(), BluetoothFailure>>,
    ),
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn sender() -> Result<tokio::sync::mpsc::UnboundedSender<Command>, BluetoothFailure> {
    static SLOT: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Command>>> = Mutex::new(None);
    let mut slot = SLOT.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(tx) = slot.clone()
        && !tx.is_closed()
    {
        return Ok(tx);
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let spawned = std::thread::Builder::new()
        .name("w3cos-bluetooth".into())
        .spawn(move || run_worker(rx))
        .is_ok();
    if !spawned {
        return Err(super::fail(
            "NetworkError",
            "bluetooth worker failed to start",
        ));
    }
    *slot = Some(tx.clone());
    Ok(tx)
}

fn call<T>(
    build: impl FnOnce(std::sync::mpsc::Sender<Result<T, BluetoothFailure>>) -> Command,
) -> Result<T, BluetoothFailure> {
    let tx = sender()?;
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    tx.send(build(reply_tx))
        .map_err(|_| super::fail("NetworkError", "bluetooth worker stopped"))?;
    match reply_rx.recv_timeout(CALL_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(super::fail("NetworkError", "bluetooth operation timed out"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(super::fail("NetworkError", "bluetooth worker stopped"))
        }
    }
}

fn run_worker(mut rx: tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    runtime.block_on(async move {
        let shared = Arc::new(Mutex::new(Shared {
            adapter: None,
            devices: HashMap::new(),
            platform_ids: HashMap::new(),
            watched: HashSet::new(),
            suppress: HashSet::new(),
            events_started: false,
        }));
        while let Some(command) = rx.recv().await {
            dispatch(Arc::clone(&shared), command).await;
        }
    });
}

async fn dispatch(shared: Arc<Mutex<Shared>>, command: Command) {
    match command {
        Command::Available(reply) => {
            let _ = reply.send(Ok(adapter_is_powered(&shared).await));
        }
        Command::Scan {
            filters,
            accept_all,
            complete,
            reply,
        } => {
            let _ = reply.send(scan_devices(&shared, &filters, accept_all, complete).await);
        }
        Command::Connect(id, reply) => {
            let _ = reply.send(connect_device(&shared, &id).await);
        }
        Command::Disconnect(id, reply) => {
            let _ = reply.send(disconnect_device(&shared, &id).await);
        }
        Command::Services(id, reply) => {
            let _ = reply.send(device_services(&shared, &id).await);
        }
        Command::Read(id, service, characteristic, reply) => {
            let _ = reply.send(read_value(&shared, &id, &service, &characteristic).await);
        }
        Command::Write(id, service, characteristic, value, with_response, reply) => {
            let _ = reply.send(
                write_value(
                    &shared,
                    &id,
                    &service,
                    &characteristic,
                    &value,
                    with_response,
                )
                .await,
            );
        }
        Command::Notify(id, service, characteristic, enable, reply) => {
            let _ = reply.send(notify_value(&shared, &id, &service, &characteristic, enable).await);
        }
        Command::ReadDescriptor(id, service, characteristic, descriptor, reply) => {
            let _ = reply.send(
                read_descriptor_value(&shared, &id, &service, &characteristic, &descriptor).await,
            );
        }
        Command::WriteDescriptor(id, service, characteristic, descriptor, value, reply) => {
            let _ = reply.send(
                write_descriptor_value(
                    &shared,
                    &id,
                    &service,
                    &characteristic,
                    &descriptor,
                    &value,
                )
                .await,
            );
        }
    }
}

pub fn available() -> bool {
    call(Command::Available).unwrap_or(false)
}

pub fn scan(
    filters: &[Filter],
    accept_all: bool,
    complete: bool,
) -> Result<Vec<Advertisement>, BluetoothFailure> {
    let filters = filters.to_vec();
    call(move |reply| Command::Scan {
        filters,
        accept_all,
        complete,
        reply,
    })
}

pub fn connect(id: &str) -> Result<(), BluetoothFailure> {
    let id = id.to_string();
    call(|reply| Command::Connect(id, reply))
}

pub fn disconnect(id: &str) -> Result<(), BluetoothFailure> {
    let id = id.to_string();
    call(|reply| Command::Disconnect(id, reply))
}

pub fn services(id: &str) -> Result<Vec<GattService>, BluetoothFailure> {
    let id = id.to_string();
    call(|reply| Command::Services(id, reply))
}

pub fn read(id: &str, service: &str, characteristic: &str) -> Result<Vec<u8>, BluetoothFailure> {
    let id = id.to_string();
    let service = service.to_string();
    let characteristic = characteristic.to_string();
    call(|reply| Command::Read(id, service, characteristic, reply))
}

pub fn write(
    id: &str,
    service: &str,
    characteristic: &str,
    value: &[u8],
    with_response: bool,
) -> Result<(), BluetoothFailure> {
    let id = id.to_string();
    let service = service.to_string();
    let characteristic = characteristic.to_string();
    let value = value.to_vec();
    call(move |reply| Command::Write(id, service, characteristic, value, with_response, reply))
}

pub fn set_notify(
    id: &str,
    service: &str,
    characteristic: &str,
    enable: bool,
) -> Result<(), BluetoothFailure> {
    let id = id.to_string();
    let service = service.to_string();
    let characteristic = characteristic.to_string();
    call(move |reply| Command::Notify(id, service, characteristic, enable, reply))
}

pub fn read_descriptor(
    id: &str,
    service: &str,
    characteristic: &str,
    descriptor: &str,
) -> Result<Vec<u8>, BluetoothFailure> {
    let id = id.to_string();
    let service = service.to_string();
    let characteristic = characteristic.to_string();
    let descriptor = descriptor.to_string();
    call(move |reply| Command::ReadDescriptor(id, service, characteristic, descriptor, reply))
}

pub fn write_descriptor(
    id: &str,
    service: &str,
    characteristic: &str,
    descriptor: &str,
    value: &[u8],
) -> Result<(), BluetoothFailure> {
    let id = id.to_string();
    let service = service.to_string();
    let characteristic = characteristic.to_string();
    let descriptor = descriptor.to_string();
    let value = value.to_vec();
    call(move |reply| {
        Command::WriteDescriptor(id, service, characteristic, descriptor, value, reply)
    })
}

fn map_bt(error: btleplug::Error) -> BluetoothFailure {
    let name = match &error {
        btleplug::Error::NoAdapterAvailable
        | btleplug::Error::DeviceNotFound
        | btleplug::Error::NoSuchCharacteristic => "NotFoundError",
        btleplug::Error::PermissionDenied => "NotAllowedError",
        btleplug::Error::NotSupported(_) => "NotSupportedError",
        _ => "NetworkError",
    };
    super::fail(name, error.to_string())
}

async fn adapter_is_powered(shared: &Arc<Mutex<Shared>>) -> bool {
    let Ok(adapter) = ensure_adapter(shared).await else {
        return false;
    };
    matches!(adapter.adapter_state().await, Ok(CentralState::PoweredOn))
}

async fn ensure_adapter(
    shared: &Arc<Mutex<Shared>>,
) -> Result<btleplug::platform::Adapter, BluetoothFailure> {
    if let Some(adapter) = lock(shared).adapter.clone() {
        return Ok(adapter);
    }
    let manager = Manager::new().await.map_err(map_bt)?;
    let adapters = manager.adapters().await.map_err(map_bt)?;
    let Some(first) = adapters.first().cloned() else {
        return Err(super::fail(
            "NotFoundError",
            "Bluetooth adapter is not available",
        ));
    };
    let mut chosen = first;
    for adapter in &adapters {
        if matches!(adapter.adapter_state().await, Ok(CentralState::PoweredOn)) {
            chosen = adapter.clone();
            break;
        }
    }
    lock(shared).adapter = Some(chosen.clone());
    start_events(shared).await;
    Ok(chosen)
}

async fn start_events(shared: &Arc<Mutex<Shared>>) {
    let adapter = {
        let guard = lock(shared);
        if guard.events_started {
            return;
        }
        guard.adapter.clone()
    };
    let Some(adapter) = adapter else {
        return;
    };
    let Ok(mut events) = adapter.events().await else {
        return;
    };
    lock(shared).events_started = true;
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let CentralEvent::DeviceDisconnected(id) = event {
                let device_id = lock(&shared).platform_ids.get(&id).cloned();
                let Some(device_id) = device_id else {
                    continue;
                };
                let suppressed = lock(&shared).suppress.remove(&device_id);
                if !suppressed {
                    super::push_remote_disconnect(device_id);
                }
            }
        }
    });
}

fn device_key(peripheral: &Peripheral) -> String {
    let bytes: [u8; 6] = peripheral.address().into();
    if bytes != [0; 6] {
        format!("ble:{:x}", peripheral.address())
    } else {
        format!("ble:{}", peripheral.id())
    }
}

fn remember(shared: &Mutex<Shared>, peripheral: &Peripheral) -> String {
    let id = device_key(peripheral);
    let mut guard = lock(shared);
    guard.platform_ids.insert(peripheral.id(), id.clone());
    guard.devices.insert(id.clone(), peripheral.clone());
    id
}

fn watch_notifications(shared: &Arc<Mutex<Shared>>, id: &str) {
    let peripheral = {
        let mut guard = lock(shared);
        if !guard.watched.insert(id.to_string()) {
            return;
        }
        guard.devices.get(id).cloned()
    };
    let Some(peripheral) = peripheral else {
        return;
    };
    let device_id = id.to_string();
    tokio::spawn(async move {
        let Ok(mut notes) = peripheral.notifications().await else {
            return;
        };
        while let Some(note) = notes.next().await {
            super::push_remote_notification(
                device_id.clone(),
                note.service_uuid.to_string(),
                note.uuid.to_string(),
                note.value,
            );
        }
    });
}

async fn lookup(shared: &Arc<Mutex<Shared>>, id: &str) -> Result<Peripheral, BluetoothFailure> {
    if let Some(peripheral) = lock(shared).devices.get(id).cloned() {
        return Ok(peripheral);
    }
    let adapter = ensure_adapter(shared).await?;
    let peripherals = adapter.peripherals().await.map_err(map_bt)?;
    for peripheral in peripherals {
        let key = remember(shared, &peripheral);
        watch_notifications(shared, &key);
        if key == id {
            return Ok(peripheral);
        }
    }
    Err(super::fail(
        "NetworkError",
        "Bluetooth device is not in range",
    ))
}

async fn advertisement(peripheral: &Peripheral, id: &str) -> Advertisement {
    let props = peripheral.properties().await.ok().flatten();
    let name = props.as_ref().and_then(|props| {
        props
            .local_name
            .clone()
            .filter(|name| !name.is_empty())
            .or_else(|| {
                props
                    .advertisement_name
                    .clone()
                    .filter(|name| !name.is_empty())
            })
    });
    let (services, manufacturer_data) = match props {
        Some(props) => (
            props.services.iter().map(ToString::to_string).collect(),
            props.manufacturer_data,
        ),
        None => (Vec::new(), HashMap::new()),
    };
    Advertisement {
        id: id.to_string(),
        name,
        services,
        manufacturer_data,
    }
}

async fn scan_devices(
    shared: &Arc<Mutex<Shared>>,
    filters: &[Filter],
    accept_all: bool,
    complete: bool,
) -> Result<Vec<Advertisement>, BluetoothFailure> {
    let adapter = ensure_adapter(shared).await?;
    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(map_bt)?;
    let outcome = collect(shared, &adapter, filters, accept_all, complete).await;
    let _ = adapter.stop_scan().await;
    outcome
}

async fn collect(
    shared: &Arc<Mutex<Shared>>,
    adapter: &btleplug::platform::Adapter,
    filters: &[Filter],
    accept_all: bool,
    complete: bool,
) -> Result<Vec<Advertisement>, BluetoothFailure> {
    let started = std::time::Instant::now();
    loop {
        let peripherals = adapter.peripherals().await.map_err(map_bt)?;
        let mut ads = Vec::new();
        for peripheral in peripherals {
            let id = remember(shared, &peripheral);
            watch_notifications(shared, &id);
            ads.push(advertisement(&peripheral, &id).await);
        }
        let elapsed = started.elapsed();
        let matched = super::scan_satisfied(&ads, filters, accept_all);
        if !complete && matched && elapsed >= SCAN_EARLY {
            return Ok(ads);
        }
        if elapsed >= SCAN_LIMIT {
            return Ok(ads);
        }
        let pause = if !complete && matched {
            SCAN_EARLY.saturating_sub(elapsed)
        } else {
            Duration::from_millis(150)
        };
        tokio::time::sleep(
            pause
                .min(Duration::from_millis(150))
                .max(Duration::from_millis(20)),
        )
        .await;
    }
}

async fn connect_device(shared: &Arc<Mutex<Shared>>, id: &str) -> Result<(), BluetoothFailure> {
    let peripheral = lookup(shared, id).await?;
    watch_notifications(shared, id);
    peripheral
        .connect_with_timeout(CONNECT_TIMEOUT)
        .await
        .map_err(map_bt)?;
    peripheral
        .discover_services_with_timeout(DISCOVER_TIMEOUT)
        .await
        .map_err(map_bt)?;
    Ok(())
}

async fn disconnect_device(shared: &Arc<Mutex<Shared>>, id: &str) -> Result<(), BluetoothFailure> {
    let peripheral = match lookup(shared, id).await {
        Ok(peripheral) => peripheral,
        Err(error) if error.name == "NetworkError" => return Ok(()),
        Err(error) => return Err(error),
    };
    lock(shared).suppress.insert(id.to_string());
    peripheral.disconnect().await.map_err(map_bt)?;
    super::push_remote_disconnect(id.to_string());
    let shared = Arc::clone(shared);
    let id = id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        lock(&shared).suppress.remove(&id);
    });
    Ok(())
}

async fn prepare(peripheral: &Peripheral) -> Result<(), BluetoothFailure> {
    if peripheral.services().is_empty() {
        peripheral
            .discover_services_with_timeout(DISCOVER_TIMEOUT)
            .await
            .map_err(map_bt)?;
    }
    Ok(())
}

fn parse_uuid(text: &str) -> Result<Uuid, BluetoothFailure> {
    Uuid::parse_str(text)
        .map_err(|_| super::fail("TypeError", format!("Unknown Bluetooth UUID: {text}")))
}

fn find_characteristic(
    peripheral: &Peripheral,
    service: &str,
    characteristic: &str,
) -> Result<btleplug::api::Characteristic, BluetoothFailure> {
    let service = parse_uuid(service)?;
    let characteristic = parse_uuid(characteristic)?;
    peripheral
        .services()
        .into_iter()
        .flat_map(|service| service.characteristics.into_iter())
        .find(|item| item.service_uuid == service && item.uuid == characteristic)
        .ok_or_else(|| super::fail("NotFoundError", "GATT characteristic not found"))
}

fn find_descriptor(
    peripheral: &Peripheral,
    service: &str,
    characteristic: &str,
    descriptor: &str,
) -> Result<btleplug::api::Descriptor, BluetoothFailure> {
    let descriptor = parse_uuid(descriptor)?;
    let characteristic = find_characteristic(peripheral, service, characteristic)?;
    characteristic
        .descriptors
        .into_iter()
        .find(|item| item.uuid == descriptor)
        .ok_or_else(|| super::fail("NotFoundError", "GATT descriptor not found"))
}

fn props(flags: CharPropFlags) -> CharProps {
    CharProps {
        broadcast: flags.contains(CharPropFlags::BROADCAST),
        read: flags.contains(CharPropFlags::READ),
        write_without_response: flags.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE),
        write: flags.contains(CharPropFlags::WRITE),
        notify: flags.contains(CharPropFlags::NOTIFY),
        indicate: flags.contains(CharPropFlags::INDICATE),
        authenticated_signed_writes: flags.contains(CharPropFlags::AUTHENTICATED_SIGNED_WRITES),
        reliable_write: false,
        writable_auxiliaries: false,
    }
}

async fn device_services(
    shared: &Arc<Mutex<Shared>>,
    id: &str,
) -> Result<Vec<GattService>, BluetoothFailure> {
    let peripheral = lookup(shared, id).await?;
    prepare(&peripheral).await?;
    let mut list = Vec::new();
    for service in peripheral.services() {
        let characteristics = service
            .characteristics
            .into_iter()
            .map(|characteristic| GattCharacteristic {
                uuid: characteristic.uuid.to_string(),
                properties: props(characteristic.properties),
                descriptors: characteristic
                    .descriptors
                    .into_iter()
                    .map(|descriptor| GattDescriptor {
                        uuid: descriptor.uuid.to_string(),
                    })
                    .collect(),
            })
            .collect();
        list.push(GattService {
            uuid: service.uuid.to_string(),
            primary: service.primary,
            characteristics,
        });
    }
    Ok(list)
}

async fn read_value(
    shared: &Arc<Mutex<Shared>>,
    id: &str,
    service: &str,
    characteristic: &str,
) -> Result<Vec<u8>, BluetoothFailure> {
    let peripheral = lookup(shared, id).await?;
    prepare(&peripheral).await?;
    let characteristic = find_characteristic(&peripheral, service, characteristic)?;
    peripheral.read(&characteristic).await.map_err(map_bt)
}

async fn write_value(
    shared: &Arc<Mutex<Shared>>,
    id: &str,
    service: &str,
    characteristic: &str,
    value: &[u8],
    with_response: bool,
) -> Result<(), BluetoothFailure> {
    let peripheral = lookup(shared, id).await?;
    prepare(&peripheral).await?;
    let characteristic = find_characteristic(&peripheral, service, characteristic)?;
    let write_type = if with_response {
        WriteType::WithResponse
    } else {
        WriteType::WithoutResponse
    };
    peripheral
        .write(&characteristic, value, write_type)
        .await
        .map_err(map_bt)
}

async fn notify_value(
    shared: &Arc<Mutex<Shared>>,
    id: &str,
    service: &str,
    characteristic: &str,
    enable: bool,
) -> Result<(), BluetoothFailure> {
    let peripheral = lookup(shared, id).await?;
    prepare(&peripheral).await?;
    let characteristic = find_characteristic(&peripheral, service, characteristic)?;
    if enable {
        peripheral.subscribe(&characteristic).await.map_err(map_bt)
    } else {
        peripheral
            .unsubscribe(&characteristic)
            .await
            .map_err(map_bt)
    }
}

async fn read_descriptor_value(
    shared: &Arc<Mutex<Shared>>,
    id: &str,
    service: &str,
    characteristic: &str,
    descriptor: &str,
) -> Result<Vec<u8>, BluetoothFailure> {
    let peripheral = lookup(shared, id).await?;
    prepare(&peripheral).await?;
    let descriptor = find_descriptor(&peripheral, service, characteristic, descriptor)?;
    peripheral
        .read_descriptor(&descriptor)
        .await
        .map_err(map_bt)
}

async fn write_descriptor_value(
    shared: &Arc<Mutex<Shared>>,
    id: &str,
    service: &str,
    characteristic: &str,
    descriptor: &str,
    value: &[u8],
) -> Result<(), BluetoothFailure> {
    let peripheral = lookup(shared, id).await?;
    prepare(&peripheral).await?;
    let descriptor = find_descriptor(&peripheral, service, characteristic, descriptor)?;
    peripheral
        .write_descriptor(&descriptor, value)
        .await
        .map_err(map_bt)
}
