//! Camera capture for `getUserMedia({video})`.
//!
//! Desktop targets open a real device on a dedicated thread and keep frames
//! flowing until the last track stops:
//!
//! - Linux (not OHOS) uses the in-tree Video4Linux2 ioctl adapter. It links
//!   no camera library.
//! - macOS uses AVFoundation through `nokhwa` (`input-avfoundation`) when
//!   `web-media-advanced` is enabled. `AVAuthorizationStatus` is checked
//!   first; denied or restricted access rejects `NotAllowedError`. The host
//!   `Info.plist` must contain `NSCameraUsageDescription`, or macOS will not
//!   grant camera access.
//! - Windows uses Media Foundation through `nokhwa` (`input-msmf`) with the
//!   same feature. A privacy-setting denial surfaces as `NotAllowedError`.
//!
//! Android, iOS, and OHOS have no camera adapter. Unit tests never open a
//! device. [`set_devices`] installs a mock camera and [`push_frame`] feeds
//! it. Hardware capture is opt-in via `W3COS_CAMERA_DEVICE_TESTS=1`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    User,
    Environment,
    Unknown,
}

impl Facing {
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Facing::User => Some("user"),
            Facing::Environment => Some("environment"),
            Facing::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Mock,
    V4l2,
    AvFoundation,
    MediaFoundation,
}

/// One supported capture window. A discrete mode has equal min and max.
/// `min_fps`/`max_fps` of `0` means the device did not report a frame rate.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoMode {
    pub min_width: u32,
    pub max_width: u32,
    pub step_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub step_height: u32,
    pub min_fps: f64,
    pub max_fps: f64,
}

impl VideoMode {
    pub fn discrete(width: u32, height: u32, fps: f64) -> Self {
        Self {
            min_width: width,
            max_width: width,
            step_width: 1,
            min_height: height,
            max_height: height,
            step_height: 1,
            min_fps: fps,
            max_fps: fps,
        }
    }

    pub fn size_range(min_width: u32, max_width: u32, min_height: u32, max_height: u32) -> Self {
        Self {
            min_width,
            max_width,
            step_width: 1,
            min_height,
            max_height,
            step_height: 1,
            min_fps: 0.0,
            max_fps: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraDevice {
    pub label: String,
    pub device_key: String,
    pub group_key: String,
    pub path: PathBuf,
    pub facing: Facing,
    pub modes: Vec<VideoMode>,
    pub index: u32,
    pub backend: Backend,
}

/// What `getUserMedia` asked for. A bare or `ideal` value is a preference;
/// `exact`, `min`, and `max` are mandatory.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoPrefs {
    pub width: Option<u32>,
    pub width_exact: bool,
    pub width_min: Option<u32>,
    pub width_max: Option<u32>,
    pub height: Option<u32>,
    pub height_exact: bool,
    pub height_min: Option<u32>,
    pub height_max: Option<u32>,
    pub frame_rate: Option<f64>,
    pub frame_rate_exact: bool,
    pub frame_rate_min: Option<f64>,
    pub frame_rate_max: Option<f64>,
    pub facing: Option<Facing>,
    pub facing_exact: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraError {
    pub name: &'static str,
    pub message: String,
    pub constraint: Option<&'static str>,
}

impl CameraError {
    pub fn named(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
            constraint: None,
        }
    }

    pub fn overconstrained(constraint: &'static str, message: impl Into<String>) -> Self {
        Self {
            name: "OverconstrainedError",
            message: message.into(),
            constraint: Some(constraint),
        }
    }

    fn not_allowed(message: impl Into<String>) -> Self {
        Self::named("NotAllowedError", message)
    }

    fn not_readable(message: impl Into<String>) -> Self {
        Self::named("NotReadableError", message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::named("NotFoundError", message)
    }
}

/// Returned by the one-shot Linux grab. Live tracks use [`FrameHub`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Still {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Negotiated {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<f64>,
    pub facing: Facing,
    pub modes: Vec<VideoMode>,
}

#[derive(Clone, Debug)]
pub struct TrackCamera {
    pub device_id: String,
    pub group_id: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<f64>,
    pub facing: Facing,
    pub modes: Vec<VideoMode>,
}

#[derive(Clone, Debug)]
pub struct DeliveredFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub timestamp_us: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformAuth {
    Authorized,
    Denied,
    Restricted,
    NotDetermined,
}

const PERMISSION_DENIED: &str = "camera access was denied or restricted; on macOS the host Info.plist must include NSCameraUsageDescription or the system refuses the camera";

#[derive(Clone)]
struct StoredFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    png: Option<Vec<u8>>,
    timestamp_us: f64,
}

struct FrameHub {
    latest: Mutex<Option<StoredFrame>>,
    pending: Mutex<VecDeque<StoredFrame>>,
    sequence: AtomicU64,
    interval_us: f64,
    cond: Condvar,
}

impl FrameHub {
    fn new(frame_rate: Option<f64>) -> Arc<Self> {
        let interval_us = match frame_rate {
            Some(fps) if fps > 1.0 => 1_000_000.0 / fps,
            _ => 33_333.0,
        };
        Arc::new(Self {
            latest: Mutex::new(None),
            pending: Mutex::new(VecDeque::new()),
            sequence: AtomicU64::new(0),
            interval_us,
            cond: Condvar::new(),
        })
    }

    fn publish(&self, width: u32, height: u32, rgba: Vec<u8>, png: Option<Vec<u8>>) {
        let pixels = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if width == 0 || height == 0 || rgba.len() < pixels {
            return;
        }
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let frame = StoredFrame {
            width,
            height,
            rgba,
            png,
            timestamp_us: sequence as f64 * self.interval_us,
        };
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            pending.push_back(frame.clone());
            while pending.len() > 4 {
                pending.pop_front();
            }
        }
        *self
            .latest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(frame);
        self.cond.notify_all();
    }

    fn publish_png(&self, width: u32, height: u32, png: Vec<u8>) {
        if let Some((decoded_w, decoded_h, rgba)) = decode_png(&png) {
            self.publish(decoded_w, decoded_h, rgba, Some(png));
        }
        let _ = (width, height);
    }

    fn drain(&self) -> Vec<DeliveredFrame> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        pending
            .drain(..)
            .filter(|frame| !frame.rgba.is_empty())
            .map(|frame| DeliveredFrame {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
                timestamp_us: frame.timestamp_us,
            })
            .collect()
    }

    fn png(&self) -> Option<(u32, u32, Vec<u8>)> {
        let mut latest = self
            .latest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let frame = latest.as_mut()?;
        if frame.png.is_none() {
            frame.png = encode_png(frame.width, frame.height, &frame.rgba).ok();
        }
        frame
            .png
            .clone()
            .map(|png| (frame.width, frame.height, png))
    }
}

struct DeviceSlot {
    device_key: String,
    hub: Arc<FrameHub>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for DeviceSlot {
    fn drop(&mut self) {
        unregister_hub(&self.device_key, &self.hub);
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

pub struct OpenedCamera {
    pub settings: Negotiated,
    slot: Arc<DeviceSlot>,
}

impl std::fmt::Debug for OpenedCamera {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenedCamera")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

struct TrackBinding {
    slot: Arc<DeviceSlot>,
    camera: TrackCamera,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthOverride {
    Authorized,
    Denied,
    Restricted,
    NotDetermined,
}

thread_local! {
    static DEVICES: std::cell::RefCell<Option<Vec<CameraDevice>>> =
        const { std::cell::RefCell::new(None) };
    static FRAME: std::cell::RefCell<Option<Still>> = const { std::cell::RefCell::new(None) };
    static PENDING: std::cell::RefCell<HashMap<String, Vec<StoredFrame>>> =
        std::cell::RefCell::new(HashMap::new());
    static TRACKS: std::cell::RefCell<HashMap<String, TrackBinding>> =
        std::cell::RefCell::new(HashMap::new());
    static AUTH: std::cell::Cell<Option<AuthOverride>> = const { std::cell::Cell::new(None) };
}

fn live_hubs() -> &'static Mutex<HashMap<String, Vec<Arc<FrameHub>>>> {
    static LIVE: std::sync::OnceLock<Mutex<HashMap<String, Vec<Arc<FrameHub>>>>> =
        std::sync::OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_hub(device_key: &str, hub: &Arc<FrameHub>) {
    live_hubs()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .entry(device_key.to_string())
        .or_default()
        .push(Arc::clone(hub));
}

fn unregister_hub(device_key: &str, hub: &Arc<FrameHub>) {
    let mut live = live_hubs()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(hubs) = live.get_mut(device_key) {
        hubs.retain(|existing| !Arc::ptr_eq(existing, hub));
        if hubs.is_empty() {
            live.remove(device_key);
        }
    }
}

fn native_enabled() -> bool {
    !cfg!(test)
        || std::env::var_os("W3COS_CAMERA_DEVICE_TESTS").is_some()
        || std::env::var_os("W3COS_CAMERA_TESTS").is_some()
}

/// Platform cameras, or the test override when one is installed.
/// `Some(vec![])` means the host has no cameras.
pub fn list_devices() -> Vec<CameraDevice> {
    if let Some(devices) = DEVICES.with(|slot| slot.borrow().clone()) {
        return devices;
    }
    native_list()
}

pub fn set_devices(devices: Option<Vec<CameraDevice>>) {
    DEVICES.with(|slot| *slot.borrow_mut() = devices);
}

/// Installs the legacy host override: cameras with no reported mode. A later
/// [`set_still_override`] supplies the frame `getUserMedia` publishes.
pub fn set_host_override(cameras: Option<Vec<HostCamera>>) {
    set_devices(cameras.map(|cameras| {
        cameras
            .into_iter()
            .map(|camera| CameraDevice {
                label: camera.label,
                device_key: camera.device_key,
                group_key: camera.group_key,
                path: camera.path,
                facing: Facing::Unknown,
                modes: Vec::new(),
                index: 0,
                backend: Backend::Mock,
            })
            .collect()
    }));
}

#[derive(Clone, Debug)]
pub struct HostCamera {
    pub label: String,
    pub device_key: String,
    pub group_key: String,
    pub path: PathBuf,
}

pub fn set_still_override(frame: Option<Still>) {
    FRAME.with(|slot| *slot.borrow_mut() = frame);
}

pub fn has_device_override() -> bool {
    DEVICES.with(|slot| slot.borrow().is_some())
}

pub fn still_override() -> Option<Still> {
    FRAME.with(|slot| slot.borrow().clone())
}

/// Replaces the platform authorization check. `Denied` and `Restricted`
/// reject `NotAllowedError` without opening a device. `None` uses the real
/// platform check (macOS `AVAuthorizationStatus`; other desktops proceed).
pub fn set_platform_authorization(auth: Option<PlatformAuth>) {
    let mapped = auth.map(|auth| match auth {
        PlatformAuth::Authorized => AuthOverride::Authorized,
        PlatformAuth::Denied => AuthOverride::Denied,
        PlatformAuth::Restricted => AuthOverride::Restricted,
        PlatformAuth::NotDetermined => AuthOverride::NotDetermined,
    });
    AUTH.with(|slot| slot.set(mapped));
}

/// Appends an RGBA frame for a mock camera. Frames pushed before the device
/// opens are delivered when it opens. Frames pushed while it is open reach
/// every live capture of that device.
pub fn push_frame(device_key: &str, width: u32, height: u32, rgba: Vec<u8>) {
    let frame = StoredFrame {
        width,
        height,
        rgba,
        png: None,
        timestamp_us: 0.0,
    };
    let hubs = live_hubs()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(device_key)
        .cloned()
        .unwrap_or_default();
    if hubs.is_empty() {
        PENDING.with(|pending| {
            pending
                .borrow_mut()
                .entry(device_key.to_string())
                .or_default()
                .push(frame);
        });
        return;
    }
    for hub in hubs {
        hub.publish(frame.width, frame.height, frame.rgba.clone(), None);
    }
}

pub fn reset_overrides() {
    DEVICES.with(|slot| *slot.borrow_mut() = None);
    FRAME.with(|slot| *slot.borrow_mut() = None);
    PENDING.with(|pending| pending.borrow_mut().clear());
    AUTH.with(|slot| slot.set(None));
}

pub fn reset_tracks() {
    TRACKS.with(|tracks| tracks.borrow_mut().clear());
}

fn is_override(device_key: &str) -> bool {
    DEVICES.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|devices| devices.iter().any(|device| device.device_key == device_key))
    })
}

fn reject_platform_auth() -> Result<(), CameraError> {
    match AUTH.with(std::cell::Cell::get) {
        Some(AuthOverride::Denied | AuthOverride::Restricted) => {
            Err(CameraError::not_allowed(PERMISSION_DENIED))
        }
        Some(AuthOverride::NotDetermined) => Err(CameraError::not_allowed(
            "camera permission is not determined; on macOS the host Info.plist must include NSCameraUsageDescription before the system can grant access",
        )),
        Some(AuthOverride::Authorized) | None => Ok(()),
    }
}

/// Checks platform camera permission before `getUserMedia` opens a device.
/// A mock override does not prompt. macOS may block in the system prompt
/// when the status is `NotDetermined`.
pub fn prepare_for_capture() -> Result<(), CameraError> {
    reject_platform_auth()?;
    if DEVICES.with(|slot| slot.borrow().is_some()) {
        return Ok(());
    }
    native_prepare()
}

pub fn open_camera(device: &CameraDevice, prefs: &VideoPrefs) -> Result<OpenedCamera, CameraError> {
    reject_platform_auth()?;
    if is_override(&device.device_key) || device.backend == Backend::Mock {
        return open_mock(device, prefs);
    }
    native_prepare()?;
    open_native(device, prefs)
}

fn open_mock(device: &CameraDevice, prefs: &VideoPrefs) -> Result<OpenedCamera, CameraError> {
    let (settings, seeded) = if device.modes.is_empty() {
        mock_unspecified(device, prefs)?
    } else {
        let negotiated = select_mode(&device.modes, device.facing, prefs)?;
        (negotiated, None)
    };
    let hub = FrameHub::new(settings.frame_rate);
    if let Some((width, height, png)) = seeded {
        hub.publish_png(width, height, png);
    }
    let pending = PENDING.with(|pending| pending.borrow_mut().remove(&device.device_key));
    if let Some(frames) = pending {
        for frame in frames {
            hub.publish(frame.width, frame.height, frame.rgba, frame.png);
        }
    }
    register_hub(&device.device_key, &hub);
    let slot = Arc::new(DeviceSlot {
        device_key: device.device_key.clone(),
        hub,
        stop: Arc::new(AtomicBool::new(false)),
        thread: Mutex::new(None),
    });
    Ok(OpenedCamera { settings, slot })
}

fn mock_unspecified(
    device: &CameraDevice,
    prefs: &VideoPrefs,
) -> Result<(Negotiated, Option<(u32, u32, Vec<u8>)>), CameraError> {
    if prefs.facing_exact && prefs.facing.is_some() && device.facing != prefs.facing.unwrap() {
        return Err(CameraError::overconstrained(
            "facingMode",
            "the camera does not report facingMode",
        ));
    }
    if prefs.frame_rate_exact || prefs.frame_rate_min.is_some() || prefs.frame_rate_max.is_some() {
        return Err(CameraError::overconstrained(
            "frameRate",
            "the camera does not report frameRate",
        ));
    }
    let still = FRAME.with(|slot| slot.borrow().clone());
    let Some(still) =
        still.filter(|still| still.width > 0 && still.height > 0 && !still.png.is_empty())
    else {
        return Err(CameraError::not_readable(format!(
            "camera override for {} has no frame",
            device.label
        )));
    };
    if !dim_fits(
        still.width,
        prefs.width,
        prefs.width_exact,
        prefs.width_min,
        prefs.width_max,
    ) {
        return Err(CameraError::overconstrained(
            "width",
            "width is outside the camera frame",
        ));
    }
    if !dim_fits(
        still.height,
        prefs.height,
        prefs.height_exact,
        prefs.height_min,
        prefs.height_max,
    ) {
        return Err(CameraError::overconstrained(
            "height",
            "height is outside the camera frame",
        ));
    }
    Ok((
        Negotiated {
            width: still.width,
            height: still.height,
            frame_rate: None,
            facing: device.facing,
            modes: Vec::new(),
        },
        Some((still.width, still.height, still.png)),
    ))
}

fn dim_fits(
    actual: u32,
    ideal: Option<u32>,
    exact: bool,
    min: Option<u32>,
    max: Option<u32>,
) -> bool {
    if exact && ideal.is_some_and(|wanted| wanted != actual) {
        return false;
    }
    if min.is_some_and(|min| actual < min) || max.is_some_and(|max| actual > max) {
        return false;
    }
    true
}

pub fn bind_track(track_id: &str, opened: OpenedCamera, device_id: String, group_id: String) {
    let camera = TrackCamera {
        device_id,
        group_id,
        width: opened.settings.width,
        height: opened.settings.height,
        frame_rate: opened.settings.frame_rate,
        facing: opened.settings.facing,
        modes: opened.settings.modes,
    };
    TRACKS.with(|tracks| {
        tracks.borrow_mut().insert(
            track_id.to_string(),
            TrackBinding {
                slot: opened.slot,
                camera,
            },
        );
    });
}

pub fn track_camera(track_id: &str) -> Option<TrackCamera> {
    TRACKS.with(|tracks| {
        tracks
            .borrow()
            .get(track_id)
            .map(|binding| binding.camera.clone())
    })
}

pub fn has_track(track_id: &str) -> bool {
    TRACKS.with(|tracks| tracks.borrow().contains_key(track_id))
}

pub fn latest_png(track_id: &str) -> Option<(u32, u32, Vec<u8>)> {
    let hub = TRACKS.with(|tracks| {
        tracks
            .borrow()
            .get(track_id)
            .map(|binding| Arc::clone(&binding.slot.hub))
    })?;
    hub.png()
}

pub fn drain_frames(track_id: &str) -> Vec<DeliveredFrame> {
    let Some(hub) = TRACKS.with(|tracks| {
        tracks
            .borrow()
            .get(track_id)
            .map(|binding| Arc::clone(&binding.slot.hub))
    }) else {
        return Vec::new();
    };
    hub.drain()
}

pub fn clone_track(from: &str, to: &str) -> bool {
    TRACKS.with(|tracks| {
        let Some(existing) = tracks.borrow().get(from).map(|binding| TrackBinding {
            slot: Arc::clone(&binding.slot),
            camera: binding.camera.clone(),
        }) else {
            return false;
        };
        tracks.borrow_mut().insert(to.to_string(), existing);
        true
    })
}

pub fn release_track(track_id: &str) {
    TRACKS.with(|tracks| tracks.borrow_mut().remove(track_id));
}

pub fn live_track_count() -> usize {
    TRACKS.with(|tracks| tracks.borrow().len())
}

/// One-shot grab used by the legacy host helper. Live tracks do not use it.
pub fn capture_still(
    path: &std::path::Path,
    width: u32,
    height: u32,
) -> Result<Still, CameraError> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        return v4l2::capture(path, width, height);
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        let _ = (path, width, height);
        Err(CameraError::not_found(
            "camera capture has no platform adapter",
        ))
    }
}

pub(crate) fn select_mode(
    modes: &[VideoMode],
    facing: Facing,
    prefs: &VideoPrefs,
) -> Result<Negotiated, CameraError> {
    if let Some(wanted) = prefs.facing {
        if prefs.facing_exact && facing != wanted {
            return Err(CameraError::overconstrained(
                "facingMode",
                if facing == Facing::Unknown {
                    "the camera does not report facingMode"
                } else {
                    "facingMode is not supported by this camera"
                },
            ));
        }
    }
    if modes.is_empty() {
        return Err(CameraError::not_readable(
            "the camera has no reported capture mode",
        ));
    }
    let mut best: Option<(u64, Negotiated)> = None;
    for mode in modes {
        let Some((width, height, frame_rate)) = resolve_mode(mode, prefs) else {
            continue;
        };
        let score = mode_distance(width, height, frame_rate, prefs);
        let negotiated = Negotiated {
            width,
            height,
            frame_rate,
            facing,
            modes: modes.to_vec(),
        };
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score < *best_score)
        {
            best = Some((score, negotiated));
        }
    }
    best.map(|(_, negotiated)| negotiated)
        .ok_or_else(|| unmet_constraint(modes, prefs))
}

fn unmet_constraint(modes: &[VideoMode], prefs: &VideoPrefs) -> CameraError {
    let width_ok = modes.iter().any(|mode| {
        resolve_dim(
            mode.min_width,
            mode.max_width,
            mode.step_width,
            prefs.width,
            prefs.width_exact,
            prefs.width_min,
            prefs.width_max,
        )
        .is_some()
    });
    if !width_ok {
        return CameraError::overconstrained(
            "width",
            "width is outside the camera's supported range",
        );
    }
    let height_ok = modes.iter().any(|mode| {
        resolve_dim(
            mode.min_width,
            mode.max_width,
            mode.step_width,
            prefs.width,
            prefs.width_exact,
            prefs.width_min,
            prefs.width_max,
        )
        .is_some()
            && resolve_dim(
                mode.min_height,
                mode.max_height,
                mode.step_height,
                prefs.height,
                prefs.height_exact,
                prefs.height_min,
                prefs.height_max,
            )
            .is_some()
    });
    if !height_ok {
        return CameraError::overconstrained(
            "height",
            "height is outside the camera's supported range",
        );
    }
    CameraError::overconstrained(
        "frameRate",
        "frameRate is outside the camera's supported range",
    )
}

fn resolve_mode(mode: &VideoMode, prefs: &VideoPrefs) -> Option<(u32, u32, Option<f64>)> {
    let width = resolve_dim(
        mode.min_width,
        mode.max_width,
        mode.step_width,
        prefs.width,
        prefs.width_exact,
        prefs.width_min,
        prefs.width_max,
    )?;
    let height = resolve_dim(
        mode.min_height,
        mode.max_height,
        mode.step_height,
        prefs.height,
        prefs.height_exact,
        prefs.height_min,
        prefs.height_max,
    )?;
    let frame_rate = resolve_fps(mode, prefs)?;
    Some((width, height, frame_rate))
}

fn resolve_dim(
    min: u32,
    max: u32,
    step: u32,
    ideal: Option<u32>,
    exact: bool,
    floor: Option<u32>,
    ceil: Option<u32>,
) -> Option<u32> {
    if min == 0 || max < min {
        return None;
    }
    let step = step.max(1);
    let low = floor.unwrap_or(min).max(min);
    let high = ceil.unwrap_or(max).min(max);
    if low > high {
        return None;
    }
    let aligned_low = align_up(low, min, step)?;
    let aligned_high = align_down(high, min, step);
    if aligned_low > high || aligned_high < aligned_low || aligned_high > max {
        return None;
    }
    let Some(wanted) = ideal else {
        return Some(aligned_high);
    };
    if wanted >= aligned_low
        && wanted <= aligned_high
        && wanted >= min
        && (wanted - min) % step == 0
    {
        return Some(wanted);
    }
    if exact {
        return None;
    }
    let snapped = snap(wanted.clamp(aligned_low, aligned_high), min, step);
    if (aligned_low..=aligned_high).contains(&snapped) {
        Some(snapped)
    } else if wanted < aligned_low {
        Some(aligned_low)
    } else {
        Some(aligned_high)
    }
}

fn align_up(value: u32, origin: u32, step: u32) -> Option<u32> {
    if value < origin {
        return Some(origin);
    }
    let offset = (value - origin) % step;
    if offset == 0 {
        Some(value)
    } else {
        value.checked_add(step - offset)
    }
}

fn align_down(value: u32, origin: u32, step: u32) -> u32 {
    if value < origin {
        return origin;
    }
    value - (value - origin) % step
}

fn snap(value: u32, origin: u32, step: u32) -> u32 {
    if value <= origin {
        return origin;
    }
    let offset = value - origin;
    let down = origin + (offset / step) * step;
    let up = down.saturating_add(step);
    if value - down <= up - value { down } else { up }
}

/// `None` rejects the mode. `Some(None)` accepts it without a known rate.
fn resolve_fps(mode: &VideoMode, prefs: &VideoPrefs) -> Option<Option<f64>> {
    if mode.min_fps <= 0.0 && mode.max_fps <= 0.0 {
        if prefs.frame_rate_exact
            || prefs.frame_rate_min.is_some()
            || prefs.frame_rate_max.is_some()
        {
            return None;
        }
        return Some(None);
    }
    let low = prefs
        .frame_rate_min
        .unwrap_or(mode.min_fps)
        .max(mode.min_fps);
    let high = prefs
        .frame_rate_max
        .unwrap_or(mode.max_fps)
        .min(mode.max_fps);
    if low > high + 0.05 {
        return None;
    }
    if let Some(wanted) = prefs.frame_rate {
        if wanted + 0.05 >= low && wanted <= high + 0.05 {
            return Some(Some(wanted.min(high).max(low)));
        }
        if prefs.frame_rate_exact {
            return None;
        }
        return Some(Some(wanted.clamp(low, high)));
    }
    Some(Some(high))
}

fn mode_distance(width: u32, height: u32, frame_rate: Option<f64>, prefs: &VideoPrefs) -> u64 {
    let width_gap = prefs
        .width
        .map(|wanted| wanted.abs_diff(width) as u64)
        .unwrap_or(0);
    let height_gap = prefs
        .height
        .map(|wanted| wanted.abs_diff(height) as u64)
        .unwrap_or(0);
    let rate_gap = match (prefs.frame_rate, frame_rate) {
        (Some(wanted), Some(got)) => (wanted - got).abs().round() as u64,
        _ => 0,
    };
    width_gap
        .saturating_mul(1_000)
        .saturating_add(height_gap.saturating_mul(1_000))
        .saturating_add(rate_gap)
}

fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let image = image::load_from_memory(bytes).ok()?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some((width, height, rgba.into_raw()))
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, CameraError> {
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| CameraError::not_readable("camera frame has the wrong number of bytes"))?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| {
            CameraError::not_readable(format!("cannot encode camera frame: {error}"))
        })?;
    Ok(png)
}

fn native_list() -> Vec<CameraDevice> {
    if !native_enabled() {
        return Vec::new();
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        return v4l2::list(std::path::Path::new("/dev"));
    }
    #[cfg(all(
        feature = "web-media-advanced",
        any(target_os = "macos", target_os = "windows")
    ))]
    {
        return nokhwa_backend::list();
    }
    #[cfg(not(any(
        all(target_os = "linux", not(target_env = "ohos")),
        all(
            feature = "web-media-advanced",
            any(target_os = "macos", target_os = "windows")
        )
    )))]
    {
        Vec::new()
    }
}

fn native_prepare() -> Result<(), CameraError> {
    #[cfg(all(
        feature = "web-media-advanced",
        any(target_os = "macos", target_os = "windows")
    ))]
    {
        return nokhwa_backend::ensure_permission();
    }
    #[cfg(not(all(
        feature = "web-media-advanced",
        any(target_os = "macos", target_os = "windows")
    )))]
    {
        Ok(())
    }
}

fn open_native(device: &CameraDevice, prefs: &VideoPrefs) -> Result<OpenedCamera, CameraError> {
    let settings = planned(device, prefs)?;
    let hub = FrameHub::new(settings.frame_rate);
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let device_key = device.device_key.clone();
    let device = device.clone();
    let prefs = prefs.clone();
    let stop_for_thread = Arc::clone(&stop);
    let hub_for_thread = Arc::clone(&hub);
    let handle = std::thread::spawn(move || {
        run_capture(device, prefs, stop_for_thread, hub_for_thread, ready_tx);
    });
    let settings = match ready_rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(settings)) => settings,
        Ok(Err(error)) => {
            stop.store(true, Ordering::Release);
            let _ = handle.join();
            return Err(error);
        }
        Err(_) => {
            stop.store(true, Ordering::Release);
            let _ = handle.join();
            return Err(CameraError::not_readable(
                "the camera produced no frame within 8 s",
            ));
        }
    };
    register_hub(&device_key, &hub);
    let slot = Arc::new(DeviceSlot {
        device_key,
        hub,
        stop,
        thread: Mutex::new(Some(handle)),
    });
    Ok(OpenedCamera { settings, slot })
}

fn planned(device: &CameraDevice, prefs: &VideoPrefs) -> Result<Negotiated, CameraError> {
    if device.modes.is_empty() {
        if prefs.facing_exact && prefs.facing.is_some() && device.facing != prefs.facing.unwrap() {
            return Err(CameraError::overconstrained(
                "facingMode",
                "the camera does not report facingMode",
            ));
        }
        return Ok(Negotiated {
            width: prefs.width.unwrap_or(640),
            height: prefs.height.unwrap_or(480),
            frame_rate: if prefs.frame_rate_exact {
                prefs.frame_rate
            } else {
                prefs.frame_rate
            },
            facing: device.facing,
            modes: Vec::new(),
        });
    }
    select_mode(&device.modes, device.facing, prefs)
}

fn run_capture(
    device: CameraDevice,
    prefs: VideoPrefs,
    stop: Arc<AtomicBool>,
    hub: Arc<FrameHub>,
    ready: std::sync::mpsc::Sender<Result<Negotiated, CameraError>>,
) {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    if device.backend == Backend::V4l2 {
        v4l2_loop(device, prefs, stop, hub, ready);
        return;
    }
    #[cfg(all(
        feature = "web-media-advanced",
        any(target_os = "macos", target_os = "windows")
    ))]
    if matches!(
        device.backend,
        Backend::AvFoundation | Backend::MediaFoundation
    ) {
        nokhwa_backend::capture_loop(device, prefs, stop, hub, ready);
        return;
    }
    let _ = (device, prefs, stop, hub);
    let _ = ready.send(Err(CameraError::not_found(
        "this build has no native camera adapter for the device",
    )));
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn v4l2_loop(
    device: CameraDevice,
    prefs: VideoPrefs,
    stop: Arc<AtomicBool>,
    hub: Arc<FrameHub>,
    ready: std::sync::mpsc::Sender<Result<Negotiated, CameraError>>,
) {
    let planned = match planned(&device, &prefs) {
        Ok(planned) => planned,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut stream = match v4l2::Stream::open(
        &device.path,
        planned.width,
        planned.height,
        planned.frame_rate,
    ) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let (width, height) = stream.size();
    let frame_rate = stream.fps().or(planned.frame_rate);
    if let Err(error) = verify_negotiated(&prefs, width, height, frame_rate) {
        let _ = ready.send(Err(error));
        return;
    }
    let negotiated = Negotiated {
        width,
        height,
        frame_rate,
        facing: device.facing,
        modes: if device.modes.is_empty() {
            vec![VideoMode::discrete(
                width,
                height,
                frame_rate.unwrap_or(0.0),
            )]
        } else {
            device.modes.clone()
        },
    };
    match stream.next_frame() {
        Ok((frame_w, frame_h, rgba)) => hub.publish(frame_w, frame_h, rgba, None),
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    }
    let _ = ready.send(Ok(negotiated));
    while !stop.load(Ordering::Acquire) {
        match stream.next_frame() {
            Ok((frame_w, frame_h, rgba)) => hub.publish(frame_w, frame_h, rgba, None),
            Err(_) if stop.load(Ordering::Acquire) => break,
            Err(_) => break,
        }
    }
}

fn verify_negotiated(
    prefs: &VideoPrefs,
    width: u32,
    height: u32,
    frame_rate: Option<f64>,
) -> Result<(), CameraError> {
    if !dim_fits(
        width,
        prefs.width,
        prefs.width_exact,
        prefs.width_min,
        prefs.width_max,
    ) {
        return Err(CameraError::overconstrained(
            "width",
            "the camera negotiated a width outside the constraint",
        ));
    }
    if !dim_fits(
        height,
        prefs.height,
        prefs.height_exact,
        prefs.height_min,
        prefs.height_max,
    ) {
        return Err(CameraError::overconstrained(
            "height",
            "the camera negotiated a height outside the constraint",
        ));
    }
    if prefs.frame_rate_exact || prefs.frame_rate_min.is_some() || prefs.frame_rate_max.is_some() {
        let Some(got) = frame_rate else {
            return Err(CameraError::overconstrained(
                "frameRate",
                "the camera did not report frameRate",
            ));
        };
        if prefs.frame_rate_exact
            && prefs
                .frame_rate
                .is_some_and(|wanted| (wanted - got).abs() > 1.0)
        {
            return Err(CameraError::overconstrained(
                "frameRate",
                "the camera negotiated a different frameRate",
            ));
        }
        if prefs.frame_rate_min.is_some_and(|min| got + 0.05 < min)
            || prefs.frame_rate_max.is_some_and(|max| got > max + 0.05)
        {
            return Err(CameraError::overconstrained(
                "frameRate",
                "the camera negotiated a frameRate outside the constraint",
            ));
        }
    }
    Ok(())
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
#[path = "camera_v4l2.rs"]
mod v4l2;

#[cfg(all(
    feature = "web-media-advanced",
    any(target_os = "macos", target_os = "windows")
))]
#[path = "camera_nokhwa.rs"]
mod nokhwa_backend;

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, fps: f64) -> VideoMode {
        VideoMode::discrete(width, height, fps)
    }

    #[test]
    fn exact_constraints_reject_and_ideals_fall_back() {
        let modes = vec![mode(640, 480, 30.0), mode(1280, 720, 30.0)];
        let exact = VideoPrefs {
            width: Some(1920),
            width_exact: true,
            ..VideoPrefs::default()
        };
        let error = select_mode(&modes, Facing::User, &exact).unwrap_err();
        assert_eq!(error.name, "OverconstrainedError");
        assert_eq!(error.constraint, Some("width"));

        let rate = VideoPrefs {
            frame_rate: Some(60.0),
            frame_rate_exact: true,
            ..VideoPrefs::default()
        };
        let error = select_mode(&modes, Facing::User, &rate).unwrap_err();
        assert_eq!(error.constraint, Some("frameRate"));

        let facing = VideoPrefs {
            facing: Some(Facing::Environment),
            facing_exact: true,
            ..VideoPrefs::default()
        };
        let error = select_mode(&modes, Facing::User, &facing).unwrap_err();
        assert_eq!(error.constraint, Some("facingMode"));

        let unknown = select_mode(&modes, Facing::Unknown, &facing).unwrap_err();
        assert_eq!(unknown.constraint, Some("facingMode"));

        let ideal = VideoPrefs {
            width: Some(1920),
            frame_rate: Some(60.0),
            facing: Some(Facing::Environment),
            ..VideoPrefs::default()
        };
        let chosen = select_mode(&modes, Facing::User, &ideal).unwrap();
        assert_eq!((chosen.width, chosen.height), (1280, 720));
        assert_eq!(chosen.frame_rate, Some(30.0));

        let exact_fit = VideoPrefs {
            width: Some(640),
            width_exact: true,
            height: Some(480),
            height_exact: true,
            frame_rate: Some(30.0),
            frame_rate_exact: true,
            ..VideoPrefs::default()
        };
        let chosen = select_mode(&modes, Facing::User, &exact_fit).unwrap();
        assert_eq!((chosen.width, chosen.height), (640, 480));
    }

    #[test]
    fn ranged_mode_snaps_to_a_legal_size() {
        let modes = vec![VideoMode {
            min_width: 320,
            max_width: 1280,
            step_width: 160,
            min_height: 240,
            max_height: 720,
            step_height: 1,
            min_fps: 15.0,
            max_fps: 30.0,
        }];
        let ideal = VideoPrefs {
            width: Some(700),
            height: Some(400),
            frame_rate: Some(24.0),
            ..VideoPrefs::default()
        };
        let chosen = select_mode(&modes, Facing::Unknown, &ideal).unwrap();
        assert_eq!(chosen.width, 640);
        assert_eq!(chosen.height, 400);
        assert_eq!(chosen.frame_rate, Some(24.0));

        let too_small = VideoPrefs {
            width: Some(160),
            width_exact: true,
            ..VideoPrefs::default()
        };
        assert_eq!(
            select_mode(&modes, Facing::Unknown, &too_small)
                .unwrap_err()
                .constraint,
            Some("width")
        );
    }

    fn mock_device() -> CameraDevice {
        CameraDevice {
            label: "Front Camera".into(),
            device_key: "mock:lifecycle".into(),
            group_key: "mock:group".into(),
            path: PathBuf::new(),
            facing: Facing::User,
            modes: vec![mode(640, 480, 30.0), mode(320, 240, 15.0)],
            index: 0,
            backend: Backend::Mock,
        }
    }

    fn rgba(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        pixel
            .into_iter()
            .cycle()
            .take((width * height * 4) as usize)
            .collect()
    }

    #[test]
    fn mock_permission_lifecycle_and_frame_delivery() {
        reset_overrides();
        reset_tracks();
        set_platform_authorization(Some(PlatformAuth::Denied));
        let device = mock_device();
        set_devices(Some(vec![device.clone()]));
        let error = open_camera(&device, &VideoPrefs::default()).unwrap_err();
        assert_eq!(error.name, "NotAllowedError");
        assert!(error.message.contains("NSCameraUsageDescription"));
        assert_eq!(live_track_count(), 0);

        set_platform_authorization(Some(PlatformAuth::Authorized));
        let prefs = VideoPrefs {
            width: Some(640),
            width_exact: true,
            ..VideoPrefs::default()
        };
        push_frame("mock:lifecycle", 640, 480, rgba(640, 480, [1, 2, 3, 255]));
        let opened = open_camera(&device, &prefs).unwrap();
        assert_eq!((opened.settings.width, opened.settings.height), (640, 480));
        assert_eq!(opened.settings.frame_rate, Some(30.0));
        bind_track("track-1", opened, "device".into(), "group".into());
        assert_eq!(live_track_count(), 1);
        let png = latest_png("track-1").unwrap();
        assert_eq!((png.0, png.1), (640, 480));
        assert_eq!(&png.2[1..4], b"PNG");
        let first = drain_frames("track-1");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].rgba[0], 1);
        assert!(drain_frames("track-1").is_empty());

        push_frame("mock:lifecycle", 640, 480, rgba(640, 480, [9, 9, 9, 255]));
        let second = drain_frames("track-1");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].rgba[0], 9);
        assert!(second[0].timestamp_us > first[0].timestamp_us);

        assert!(clone_track("track-1", "track-2"));
        assert_eq!(live_track_count(), 2);
        release_track("track-1");
        assert!(latest_png("track-2").is_some());
        push_frame("mock:lifecycle", 640, 480, rgba(640, 480, [4, 4, 4, 255]));
        assert_eq!(drain_frames("track-2")[0].rgba[0], 4);
        release_track("track-2");
        assert_eq!(live_track_count(), 0);
        assert!(drain_frames("track-2").is_empty());

        let wrong = VideoPrefs {
            height: Some(10),
            height_exact: true,
            ..VideoPrefs::default()
        };
        let error = open_camera(&device, &wrong).unwrap_err();
        assert_eq!(error.constraint, Some("height"));

        reset_overrides();
        reset_tracks();
    }

    #[test]
    fn legacy_still_override_publishes_one_frame_and_rejects_a_missing_one() {
        reset_overrides();
        reset_tracks();
        set_host_override(Some(vec![HostCamera {
            label: "Integrated Camera".into(),
            device_key: "v4l2:test".into(),
            group_key: "v4l2:bus".into(),
            path: PathBuf::from("/dev/video0"),
        }]));
        let device = list_devices().remove(0);
        let missing = open_camera(&device, &VideoPrefs::default()).unwrap_err();
        assert_eq!(missing.name, "NotReadableError");
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        set_still_override(Some(Still {
            width: 4,
            height: 3,
            png,
        }));
        let opened = open_camera(
            &device,
            &VideoPrefs {
                width: Some(1280),
                ..VideoPrefs::default()
            },
        )
        .unwrap();
        assert_eq!((opened.settings.width, opened.settings.height), (4, 3));
        bind_track("legacy", opened, "id".into(), "group".into());
        assert_eq!(latest_png("legacy").unwrap().0, 4);
        release_track("legacy");
        reset_overrides();
        reset_tracks();
    }

    /// Opt-in: `W3COS_CAMERA_DEVICE_TESTS=1` opens the first real camera and
    /// reads a frame, then drops the session so the device is released.
    #[test]
    fn hardware_camera_streams_a_frame_when_enabled() {
        if std::env::var_os("W3COS_CAMERA_DEVICE_TESTS").is_none() {
            return;
        }
        reset_overrides();
        reset_tracks();
        let devices = list_devices();
        let device = devices
            .first()
            .expect("W3COS_CAMERA_DEVICE_TESTS set but no camera");
        let opened = open_camera(device, &VideoPrefs::default()).expect("open");
        assert!(opened.settings.width > 0 && opened.settings.height > 0);
        bind_track("hardware", opened, "device".into(), "group".into());
        let png = latest_png("hardware").expect("frame");
        assert!(png.0 > 0 && png.1 > 0 && png.2.len() > 8);
        assert!(!drain_frames("hardware").is_empty());
        release_track("hardware");
        assert_eq!(live_track_count(), 0);
        reset_tracks();
    }
}
