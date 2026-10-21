//! Host adapters for powerful APIs.
//!
//! Linux uses X11 / sysfs when those sources exist. A missing source returns
//! an error so callers can reject; they do not substitute a successful
//! placeholder. Tests may install an override for the happy path.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct BatteryReading {
    pub charging: bool,
    pub charging_time: f64,
    pub discharging_time: f64,
    pub level: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    pub label: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub internal: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CapturedImage {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// A video-capture device reported by the host camera adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub label: String,
    /// Stable host identity; `mediaDevices` hashes it into `deviceId`.
    pub device_key: String,
    /// Shared by devices on the same physical unit; hashed into `groupId`.
    pub group_key: String,
    pub path: PathBuf,
}

/// One captured camera frame, PNG-encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// Camera failure with the DOMException name `getUserMedia` should reject with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraError {
    pub name: &'static str,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleSample {
    pub idle_ms: u64,
    pub screen_locked: bool,
}

thread_local! {
    static BATTERY_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static DISPLAYS: RefCell<Option<Vec<DisplayInfo>>> = const { RefCell::new(None) };
    static CAPTURE: RefCell<Option<CapturedImage>> = const { RefCell::new(None) };
    static IDLE: RefCell<Option<IdleSample>> = const { RefCell::new(None) };
    static WAKE_HOOK: RefCell<Option<fn(bool) -> Result<(), String>>> = const { RefCell::new(None) };
    static WAKE_HELD: RefCell<bool> = const { RefCell::new(false) };
    static CAMERAS: RefCell<Option<Vec<CameraInfo>>> = const { RefCell::new(None) };
    static CAMERA_FRAME: RefCell<Option<CameraFrame>> = const { RefCell::new(None) };
}

pub fn reset_overrides() {
    BATTERY_ROOT.with(|slot| *slot.borrow_mut() = None);
    DISPLAYS.with(|slot| *slot.borrow_mut() = None);
    CAPTURE.with(|slot| *slot.borrow_mut() = None);
    IDLE.with(|slot| *slot.borrow_mut() = None);
    WAKE_HOOK.with(|slot| *slot.borrow_mut() = None);
    WAKE_HELD.with(|slot| *slot.borrow_mut() = false);
    CAMERAS.with(|slot| *slot.borrow_mut() = None);
    CAMERA_FRAME.with(|slot| *slot.borrow_mut() = None);
}

/// Replaces host camera enumeration (`Some(vec![])` means "no cameras").
pub fn set_camera_override(cameras: Option<Vec<CameraInfo>>) {
    CAMERAS.with(|slot| *slot.borrow_mut() = cameras);
}

/// Replaces host camera capture for overridden cameras.
pub fn set_camera_frame_override(frame: Option<CameraFrame>) {
    CAMERA_FRAME.with(|slot| *slot.borrow_mut() = frame);
}

/// Video-capture devices exposed by the platform adapter (Linux: V4L2).
pub fn list_cameras() -> Vec<CameraInfo> {
    if let Some(cameras) = CAMERAS.with(|slot| slot.borrow().clone()) {
        return cameras;
    }
    native_cameras()
}

/// Captures one frame from `camera`, preferring `width`×`height`.
pub fn capture_camera(
    camera: &CameraInfo,
    width: u32,
    height: u32,
) -> Result<CameraFrame, CameraError> {
    if CAMERAS.with(|slot| slot.borrow().is_some()) {
        return match CAMERA_FRAME.with(|slot| slot.borrow().clone()) {
            Some(frame) if frame.width > 0 && frame.height > 0 && !frame.png.is_empty() => {
                Ok(frame)
            }
            _ => Err(CameraError {
                name: "NotReadableError",
                message: format!("camera override for {} has no frame", camera.label),
            }),
        };
    }
    native_capture_camera(camera, width, height)
}

/// Native enumeration is skipped in unit tests unless
/// `W3COS_CAMERA_TESTS=1`, so a developer webcam cannot change results.
fn native_camera_enabled() -> bool {
    !cfg!(test) || std::env::var_os("W3COS_CAMERA_TESTS").is_some()
}

fn native_cameras() -> Vec<CameraInfo> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        if native_camera_enabled() {
            return v4l2::list(Path::new("/dev"));
        }
    }
    let _ = native_camera_enabled;
    Vec::new()
}

fn native_capture_camera(
    camera: &CameraInfo,
    width: u32,
    height: u32,
) -> Result<CameraFrame, CameraError> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        v4l2::capture(&camera.path, width, height)
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        let _ = (width, height);
        Err(CameraError {
            name: "NotFoundError",
            message: format!("camera {} has no platform capture adapter", camera.label),
        })
    }
}

pub fn set_battery_sysfs_root(path: Option<PathBuf>) {
    BATTERY_ROOT.with(|slot| *slot.borrow_mut() = path);
}

pub fn set_display_override(displays: Option<Vec<DisplayInfo>>) {
    DISPLAYS.with(|slot| *slot.borrow_mut() = displays);
}

pub fn set_capture_override(image: Option<CapturedImage>) {
    CAPTURE.with(|slot| *slot.borrow_mut() = image);
}

pub fn set_idle_override(sample: Option<IdleSample>) {
    IDLE.with(|slot| *slot.borrow_mut() = sample);
}

pub fn set_wake_hook(hook: Option<fn(bool) -> Result<(), String>>) {
    WAKE_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

pub fn read_battery() -> Result<BatteryReading, String> {
    let root = BATTERY_ROOT.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(|| PathBuf::from("/sys/class/power_supply"))
    });
    read_battery_from(&root)
}

fn read_battery_from(root: &Path) -> Result<BatteryReading, String> {
    let entries = std::fs::read_dir(root)
        .map_err(|err| format!("battery supply is not exposed at {}: {err}", root.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let type_name = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if !type_name.to_ascii_lowercase().contains("battery")
            && std::fs::read_to_string(path.join("capacity")).is_err()
        {
            continue;
        }
        if let Ok(reading) = reading_from_supply(&path) {
            return Ok(reading);
        }
    }
    Err(format!(
        "no battery supply is exposed at {}",
        root.display()
    ))
}

fn reading_from_supply(path: &Path) -> Result<BatteryReading, String> {
    let status = std::fs::read_to_string(path.join("status"))
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let capacity = std::fs::read_to_string(path.join("capacity"))
        .map_err(|err| err.to_string())?
        .trim()
        .parse::<f64>()
        .map_err(|err| err.to_string())?;
    let charging = matches!(status.as_str(), "charging" | "full" | "not charging" | "");
    let level = (capacity / 100.0).clamp(0.0, 1.0);
    Ok(BatteryReading {
        charging: charging || status.is_empty(),
        charging_time: if charging { 0.0 } else { f64::INFINITY },
        discharging_time: if charging { f64::INFINITY } else { 0.0 },
        level,
    })
}

pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    if let Some(displays) = DISPLAYS.with(|slot| slot.borrow().clone()) {
        return if displays.is_empty() {
            Err("display override is empty".into())
        } else {
            Ok(displays)
        };
    }
    native_displays()
}

pub fn capture_display() -> Result<CapturedImage, String> {
    if let Some(image) = CAPTURE.with(|slot| slot.borrow().clone()) {
        if image.png.is_empty() || image.width == 0 || image.height == 0 {
            return Err("capture override is empty".into());
        }
        return Ok(image);
    }
    native_capture()
}

pub fn sample_idle() -> Result<IdleSample, String> {
    if let Some(sample) = IDLE.with(|slot| *slot.borrow()) {
        return Ok(sample);
    }
    native_idle()
}

pub fn set_screen_awake(awake: bool) -> Result<(), String> {
    if let Some(hook) = WAKE_HOOK.with(|slot| *slot.borrow()) {
        hook(awake)?;
        WAKE_HELD.with(|slot| *slot.borrow_mut() = awake);
        return Ok(());
    }
    native_set_screen_awake(awake)?;
    WAKE_HELD.with(|slot| *slot.borrow_mut() = awake);
    Ok(())
}

pub fn screen_awake_held() -> bool {
    WAKE_HELD.with(|slot| *slot.borrow())
}

fn native_displays() -> Result<Vec<DisplayInfo>, String> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        if let Ok(displays) = x11::displays_from_xrandr() {
            return Ok(displays);
        }
    }
    Err("no host display enumerator is available".into())
}

fn native_capture() -> Result<CapturedImage, String> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        return x11::capture_root();
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        Err("screen capture has no platform adapter".into())
    }
}

fn native_idle() -> Result<IdleSample, String> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        return x11::idle_sample();
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        Err("idle sampling has no platform adapter".into())
    }
}

fn native_set_screen_awake(awake: bool) -> Result<(), String> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        return x11::set_screensaver_suspended(awake);
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        let _ = awake;
        Err("screen wake lock has no platform power adapter".into())
    }
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
#[path = "camera_v4l2.rs"]
mod v4l2;

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod x11 {
    use super::{CapturedImage, DisplayInfo, IdleSample};
    use std::sync::atomic::{AtomicI32, Ordering};

    const KEY_PRESS: i32 = 2;
    const SHIFT_MASK: u32 = 1;
    const LOCK_MASK: u32 = 2;
    const CONTROL_MASK: u32 = 4;
    const MOD1_MASK: u32 = 8;
    const MOD2_MASK: u32 = 16;
    const MOD4_MASK: u32 = 64;
    const GRAB_MODE_ASYNC: i32 = 1;

    static LAST_X_ERROR: AtomicI32 = AtomicI32::new(0);

    #[repr(C)]
    struct XErrorEvent {
        type_: i32,
        _pad: i32,
        display: *mut std::ffi::c_void,
        resourceid: u64,
        serial: u64,
        error_code: u8,
        request_code: u8,
        minor_code: u8,
    }

    #[repr(C)]
    struct XScreenSaverInfo {
        window: u64,
        state: i32,
        kind: i32,
        til_or_since: u64,
        idle: u64,
        event_mask: u64,
    }

    #[repr(C)]
    struct XKeyEvent {
        type_: i32,
        _pad0: i32,
        serial: u64,
        send_event: i32,
        _pad1: i32,
        display: *mut std::ffi::c_void,
        window: u64,
        root: u64,
        subwindow: u64,
        time: u64,
        x: i32,
        y: i32,
        x_root: i32,
        y_root: i32,
        state: u32,
        keycode: u32,
        same_screen: i32,
    }

    #[link(name = "X11")]
    unsafe extern "C" {
        fn XOpenDisplay(name: *const libc::c_char) -> *mut std::ffi::c_void;
        fn XCloseDisplay(display: *mut std::ffi::c_void) -> i32;
        fn XDefaultRootWindow(display: *mut std::ffi::c_void) -> u64;
        fn XGetGeometry(
            display: *mut std::ffi::c_void,
            drawable: u64,
            root: *mut u64,
            x: *mut i32,
            y: *mut i32,
            width: *mut u32,
            height: *mut u32,
            border: *mut u32,
            depth: *mut u32,
        ) -> i32;
        fn XGetImage(
            display: *mut std::ffi::c_void,
            drawable: u64,
            x: i32,
            y: i32,
            width: u32,
            height: u32,
            plane_mask: u64,
            format: i32,
        ) -> *mut std::ffi::c_void;
        fn XGetPixel(image: *mut std::ffi::c_void, x: i32, y: i32) -> u64;
        fn XDestroyImage(image: *mut std::ffi::c_void) -> i32;
        fn XSetErrorHandler(
            handler: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut XErrorEvent) -> i32>,
        ) -> Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut XErrorEvent) -> i32>;
        fn XSync(display: *mut std::ffi::c_void, discard: i32) -> i32;
        fn XFree(data: *mut std::ffi::c_void) -> i32;
        fn XStringToKeysym(name: *const libc::c_char) -> u64;
        fn XKeysymToKeycode(display: *mut std::ffi::c_void, keysym: u64) -> u8;
        fn XGrabKey(
            display: *mut std::ffi::c_void,
            keycode: i32,
            modifiers: u32,
            grab_window: u64,
            owner_events: i32,
            pointer_mode: i32,
            keyboard_mode: i32,
        ) -> i32;
        fn XUngrabKey(
            display: *mut std::ffi::c_void,
            keycode: i32,
            modifiers: u32,
            grab_window: u64,
        ) -> i32;
        fn XPending(display: *mut std::ffi::c_void) -> i32;
        fn XNextEvent(display: *mut std::ffi::c_void, event: *mut XKeyEvent) -> i32;
        fn XFlush(display: *mut std::ffi::c_void) -> i32;
    }

    #[link(name = "Xss")]
    unsafe extern "C" {
        fn XScreenSaverAllocInfo() -> *mut XScreenSaverInfo;
        fn XScreenSaverQueryInfo(
            display: *mut std::ffi::c_void,
            drawable: u64,
            info: *mut XScreenSaverInfo,
        ) -> i32;
        fn XScreenSaverSuspend(display: *mut std::ffi::c_void, suspend: i32);
    }

    unsafe extern "C" fn on_x_error(
        _display: *mut std::ffi::c_void,
        event: *mut XErrorEvent,
    ) -> i32 {
        if !event.is_null() {
            LAST_X_ERROR.store(i32::from(unsafe { (*event).error_code }), Ordering::Relaxed);
        }
        0
    }

    struct Display(*mut std::ffi::c_void);

    impl Display {
        fn open() -> Result<Self, String> {
            let display = unsafe { XOpenDisplay(std::ptr::null()) };
            if display.is_null() {
                Err("XOpenDisplay failed; DISPLAY is unset or unreachable".into())
            } else {
                unsafe {
                    XSetErrorHandler(Some(on_x_error));
                }
                Ok(Self(display))
            }
        }
    }

    impl Drop for Display {
        fn drop(&mut self) {
            unsafe {
                XCloseDisplay(self.0);
            }
        }
    }

    fn check_error(display: &Display, what: &str) -> Result<(), String> {
        unsafe {
            XSync(display.0, 0);
        }
        let code = LAST_X_ERROR.swap(0, Ordering::Relaxed);
        if code == 0 {
            Ok(())
        } else {
            Err(format!("{what} failed with X error {code}"))
        }
    }

    pub fn displays_from_xrandr() -> Result<Vec<DisplayInfo>, String> {
        let output = std::process::Command::new("xrandr")
            .arg("--query")
            .env(
                "DISPLAY",
                std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()),
            )
            .output()
            .map_err(|err| format!("xrandr is unavailable: {err}"))?;
        if !output.status.success() {
            return Err(format!(
                "xrandr failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut displays = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.next() else {
                continue;
            };
            if parts.next() != Some("connected") {
                continue;
            }
            let geometry = parts.find(|part| part.contains('x') && part.contains('+'));
            let Some(geometry) = geometry else {
                continue;
            };
            let Some((size, position)) = geometry.split_once('+') else {
                continue;
            };
            let Some((width, height)) = size.split_once('x') else {
                continue;
            };
            let (left, top) = position.split_once('+').unwrap_or((position, "0"));
            let width: u32 = width.parse().map_err(|err| format!("{err}"))?;
            let height: u32 = height.parse().map_err(|err| format!("{err}"))?;
            let left: i32 = left.parse().unwrap_or(0);
            let top: i32 = top.parse().unwrap_or(0);
            if width == 0 || height == 0 {
                continue;
            }
            displays.push(DisplayInfo {
                label: name.to_string(),
                left,
                top,
                width,
                height,
                primary: displays.is_empty(),
                internal: name.to_ascii_lowercase().contains("edp")
                    || name.to_ascii_lowercase().contains("lvds"),
            });
        }
        if displays.is_empty() {
            Err("xrandr reported no connected outputs".into())
        } else {
            Ok(displays)
        }
    }

    pub fn idle_sample() -> Result<IdleSample, String> {
        let display = Display::open()?;
        LAST_X_ERROR.store(0, Ordering::Relaxed);
        let root = unsafe { XDefaultRootWindow(display.0) };
        let info = unsafe { XScreenSaverAllocInfo() };
        if info.is_null() {
            return Err("XScreenSaverAllocInfo failed".into());
        }
        let status = unsafe { XScreenSaverQueryInfo(display.0, root, info) };
        let sample = unsafe {
            IdleSample {
                idle_ms: (*info).idle,
                screen_locked: (*info).state == 1,
            }
        };
        unsafe {
            XFree(info.cast());
        }
        if status == 0 {
            return Err("XScreenSaverQueryInfo failed".into());
        }
        check_error(&display, "XScreenSaverQueryInfo")?;
        Ok(sample)
    }

    pub fn set_screensaver_suspended(suspend: bool) -> Result<(), String> {
        let display = Display::open()?;
        LAST_X_ERROR.store(0, Ordering::Relaxed);
        unsafe {
            XScreenSaverSuspend(display.0, i32::from(suspend));
        }
        check_error(&display, "XScreenSaverSuspend")
    }

    pub fn capture_root() -> Result<CapturedImage, String> {
        let display = Display::open()?;
        LAST_X_ERROR.store(0, Ordering::Relaxed);
        let root = unsafe { XDefaultRootWindow(display.0) };
        let mut root_return = 0u64;
        let mut x = 0i32;
        let mut y = 0i32;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut border = 0u32;
        let mut depth = 0u32;
        let status = unsafe {
            XGetGeometry(
                display.0,
                root,
                &mut root_return,
                &mut x,
                &mut y,
                &mut width,
                &mut height,
                &mut border,
                &mut depth,
            )
        };
        if status == 0 || width == 0 || height == 0 {
            return Err("XGetGeometry failed for the root window".into());
        }
        let step_x = (width / 160).max(1);
        let step_y = (height / 100).max(1);
        let out_w = width.div_ceil(step_x);
        let out_h = height.div_ceil(step_y);
        let ximage = unsafe {
            XGetImage(
                display.0,
                root,
                0,
                0,
                width,
                height,
                u64::MAX,
                2, // ZPixmap
            )
        };
        if ximage.is_null() {
            return Err("XGetImage failed".into());
        }
        let mut rgba = Vec::with_capacity((out_w * out_h * 4) as usize);
        for py in 0..out_h {
            for px in 0..out_w {
                let pixel =
                    unsafe { XGetPixel(ximage, (px * step_x) as i32, (py * step_y) as i32) };
                rgba.push(((pixel >> 16) & 0xff) as u8);
                rgba.push(((pixel >> 8) & 0xff) as u8);
                rgba.push((pixel & 0xff) as u8);
                rgba.push(255);
            }
        }
        unsafe {
            XDestroyImage(ximage);
        }
        check_error(&display, "XGetImage")?;
        let mut png = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png);
        use image::ImageEncoder;
        encoder
            .write_image(&rgba, out_w, out_h, image::ExtendedColorType::Rgba8)
            .map_err(|err| format!("PNG encode failed: {err}"))?;
        Ok(CapturedImage {
            width: out_w,
            height: out_h,
            png,
        })
    }

    #[derive(Clone)]
    struct GrabSpec {
        keycode: i32,
        modifiers: u32,
        accelerator: String,
    }

    struct GrabThread {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
        events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    static GRAB: std::sync::Mutex<Option<GrabThread>> = std::sync::Mutex::new(None);

    pub fn install_grabs(accelerators: &[String]) -> Result<(), String> {
        clear_grabs();
        if accelerators.is_empty() {
            return Ok(());
        }
        let display = Display::open()?;
        let root = unsafe { XDefaultRootWindow(display.0) };
        let mut specs = Vec::new();
        for accelerator in accelerators {
            let Some((keysym_name, modifiers)) = accelerator_keysym(accelerator) else {
                return Err(format!("unsupported accelerator {accelerator}"));
            };
            let mut c_name = keysym_name.clone().into_bytes();
            c_name.push(0);
            let keysym = unsafe { XStringToKeysym(c_name.as_ptr().cast()) };
            if keysym == 0 {
                return Err(format!("unknown keysym {keysym_name}"));
            }
            let keycode = unsafe { XKeysymToKeycode(display.0, keysym) };
            if keycode == 0 {
                return Err(format!("no keycode for {keysym_name}"));
            }
            for extra in [0, LOCK_MASK, MOD2_MASK, LOCK_MASK | MOD2_MASK] {
                let mask = modifiers | extra;
                LAST_X_ERROR.store(0, Ordering::Relaxed);
                unsafe {
                    XGrabKey(
                        display.0,
                        i32::from(keycode),
                        mask,
                        root,
                        0,
                        GRAB_MODE_ASYNC,
                        GRAB_MODE_ASYNC,
                    );
                }
                check_error(&display, "XGrabKey")?;
                specs.push(GrabSpec {
                    keycode: i32::from(keycode),
                    modifiers,
                    accelerator: accelerator.clone(),
                });
            }
        }
        unsafe {
            XFlush(display.0);
        }
        // The grab is on this connection. Keep it open on the listener thread.
        // A raw pointer is not `Send`; the address is moved as `usize`.
        let raw = display.0 as usize;
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_for_thread = std::sync::Arc::clone(&events);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_thread = std::sync::Arc::clone(&stop);
        let specs = std::sync::Arc::new(specs);
        let join = std::thread::Builder::new()
            .name("w3cos-shortcut-grab".into())
            .spawn(move || {
                let display = raw as *mut std::ffi::c_void;
                grab_loop(display, root, specs, events_for_thread, stop_for_thread)
            })
            .map_err(|err| format!("shortcut listener failed to start: {err}"))?;
        std::mem::forget(display);
        if let Ok(mut slot) = GRAB.lock() {
            *slot = Some(GrabThread {
                stop,
                join: Some(join),
                events,
            });
        }
        Ok(())
    }

    fn grab_loop(
        display: *mut std::ffi::c_void,
        root: u64,
        specs: std::sync::Arc<Vec<GrabSpec>>,
        events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        while !stop.load(Ordering::Relaxed) {
            let pending = unsafe { XPending(display) };
            if pending <= 0 {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            let mut event = unsafe { std::mem::zeroed::<XKeyEvent>() };
            unsafe {
                XNextEvent(display, &mut event);
            }
            if event.type_ != KEY_PRESS {
                continue;
            }
            let state = event.state & (SHIFT_MASK | CONTROL_MASK | MOD1_MASK | MOD4_MASK);
            if let Some(spec) = specs
                .iter()
                .find(|spec| spec.keycode == event.keycode as i32 && spec.modifiers == state)
            {
                if let Ok(mut queue) = events.lock() {
                    queue.push(spec.accelerator.clone());
                }
            }
        }
        for spec in specs.iter() {
            for extra in [0, LOCK_MASK, MOD2_MASK, LOCK_MASK | MOD2_MASK] {
                unsafe {
                    XUngrabKey(display, spec.keycode, spec.modifiers | extra, root);
                }
            }
        }
        unsafe {
            XCloseDisplay(display);
        }
    }

    pub fn poll_grabs() -> Vec<String> {
        let Ok(slot) = GRAB.lock() else {
            return Vec::new();
        };
        let Some(grab) = slot.as_ref() else {
            return Vec::new();
        };
        grab.events
            .lock()
            .map(|mut events| std::mem::take(&mut *events))
            .unwrap_or_default()
    }

    pub fn clear_grabs() {
        let existing = GRAB.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut grab) = existing {
            grab.stop.store(true, Ordering::Relaxed);
            if let Some(join) = grab.join.take() {
                let _ = join.join();
            }
        }
    }

    fn accelerator_keysym(accelerator: &str) -> Option<(String, u32)> {
        let parts = accelerator
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let key = parts.last()?.to_string();
        if key.chars().count() != 1 && !key.to_ascii_lowercase().starts_with('f') {
            return None;
        }
        let mut modifiers = 0u32;
        for part in &parts[..parts.len() - 1] {
            match part.to_ascii_lowercase().as_str() {
                "control" | "ctrl" => modifiers |= CONTROL_MASK,
                "shift" => modifiers |= SHIFT_MASK,
                "alt" | "option" => modifiers |= MOD1_MASK,
                "meta" | "super" | "command" => modifiers |= MOD4_MASK,
                _ => return None,
            }
        }
        let keysym = if key.chars().count() == 1 {
            key.to_ascii_lowercase()
        } else {
            key
        };
        Some((keysym, modifiers))
    }
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
pub use x11::{clear_grabs, install_grabs, poll_grabs};

#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
pub fn install_grabs(_accelerators: &[String]) -> Result<(), String> {
    Err("global shortcut grab has no platform adapter".into())
}

#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
pub fn poll_grabs() -> Vec<String> {
    Vec::new()
}

#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
pub fn clear_grabs() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_reader_uses_sysfs_and_refuses_an_empty_supply() {
        let root = std::env::temp_dir().join(format!("w3cos-bat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("BAT0")).unwrap();
        std::fs::write(root.join("BAT0/type"), "Battery\n").unwrap();
        std::fs::write(root.join("BAT0/status"), "Discharging\n").unwrap();
        std::fs::write(root.join("BAT0/capacity"), "42\n").unwrap();
        set_battery_sysfs_root(Some(root.clone()));
        let reading = read_battery().unwrap();
        assert!(!reading.charging);
        assert!((reading.level - 0.42).abs() < 0.001);
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        set_battery_sysfs_root(Some(empty));
        assert!(read_battery().is_err());
        reset_overrides();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overrides_do_not_invent_empty_success() {
        set_display_override(Some(Vec::new()));
        assert!(list_displays().is_err());
        set_capture_override(Some(CapturedImage {
            width: 0,
            height: 1,
            png: vec![1],
        }));
        assert!(capture_display().is_err());
        reset_overrides();
    }
}
