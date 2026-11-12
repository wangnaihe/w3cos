//! Linux desktop telemetry for Generic Sensor and device motion.
//!
//! Live samples come from the Industrial I/O sysfs tree
//! (`/sys/bus/iio/devices`: `in_accel_*`, `in_anglvel_*`, `in_magn_*`,
//! `in_gravity_*`, `in_illuminance_*`, plus scale, offset, and mount matrix).
//! Magnetometer scale is Gauss and is converted to microtesla. When a raw
//! accelerometer or illuminance channel is missing, Linux may ask
//! `net.hadess.SensorProxy` on the system bus; proxy orientation is only the
//! four-way "which edge is up" reading, so linear acceleration stays unavailable.
//!
//! macOS, Windows, and mobile have no adapter here. `open_source` returns
//! [`OpenResult::Missing`] and the sensor fires `NotReadableError`. Nothing in
//! this module invents a motion trace.
//!
//! Unit tests do not touch a real device unless `W3COS_SENSOR_DEVICE_TESTS=1`.
//! A [`set_sysfs_root`] override points the sampler at a fake tree instead.
//! Each JavaScript thread owns its sampler, so parallel tests stay isolated.
//! Host injection remains the path when this feature is compiled out.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use std::cell::RefCell;

const G: f64 = 9.80665;
const MIN_HZ: f64 = 1.0;
const MAX_HZ: f64 = 200.0;
const WINDOW_ORIENTATION_ID: u64 = u64::MAX;
const WINDOW_MOTION_ID: u64 = u64::MAX - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Accelerometer,
    Gravity,
    LinearAcceleration,
    Gyroscope,
    Magnetometer,
    AmbientLight,
    AbsoluteOrientation,
    RelativeOrientation,
    WindowOrientation,
    WindowMotion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenResult {
    /// Tests (or a feature-off caller) keep the host-injection lifecycle.
    HostOnly,
    Live,
    Missing(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Sample {
    Vector {
        source: Source,
        x: f64,
        y: f64,
        z: f64,
        timestamp: f64,
    },
    Illuminance {
        lux: f64,
        timestamp: f64,
    },
    Quaternion {
        source: Source,
        quaternion: [f64; 4],
        timestamp: f64,
    },
    Motion {
        acceleration: Option<[f64; 3]>,
        including_gravity: Option<[f64; 3]>,
        rotation_alpha: Option<f64>,
        rotation_beta: Option<f64>,
        rotation_gamma: Option<f64>,
        interval_ms: f64,
    },
    Orientation {
        alpha: Option<f64>,
        beta: f64,
        gamma: f64,
        absolute: bool,
    },
    Unavailable {
        source: Source,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ProxyReading {
    orientation: Option<String>,
    lux: Option<f64>,
}

#[derive(Clone, Debug)]
struct Sub {
    id: u64,
    source: Source,
    interval: Duration,
    next_due: Instant,
    root: PathBuf,
    allow_proxy: bool,
}

#[derive(Clone, Debug, Default)]
struct Fusion {
    gravity: Option<[f64; 3]>,
    last: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    accel: Option<[f64; 3]>,
    gravity: Option<[f64; 3]>,
    gyro: Option<[f64; 3]>,
    mag: Option<[f64; 3]>,
    illuminance: Option<f64>,
    accel_from_proxy: bool,
}

struct Shared {
    stop: AtomicBool,
    running: AtomicBool,
    subs: Mutex<Vec<Sub>>,
    samples: Mutex<Vec<Sample>>,
    fusion: Mutex<Fusion>,
    time_origin: Mutex<Option<(Instant, f64)>>,
    wake: Condvar,
    wake_mutex: Mutex<()>,
}

struct Local {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

thread_local! {
    static ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static LOCAL: RefCell<Option<Local>> = const { RefCell::new(None) };
}

impl Drop for Local {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            subs: Mutex::new(Vec::new()),
            samples: Mutex::new(Vec::new()),
            fusion: Mutex::new(Fusion::default()),
            time_origin: Mutex::new(None),
            wake: Condvar::new(),
            wake_mutex: Mutex::new(()),
        })
    }
}

fn proxy_slot() -> &'static Mutex<Option<(Instant, ProxyReading)>> {
    static PROXY: OnceLock<Mutex<Option<(Instant, ProxyReading)>>> = OnceLock::new();
    PROXY.get_or_init(|| Mutex::new(None))
}

/// Point this thread's sampler at `path` instead of `/sys/bus/iio/devices`.
pub fn set_sysfs_root(path: Option<PathBuf>) {
    ROOT.with(|root| *root.borrow_mut() = path);
}

/// Stop this thread's sampler and drop queued samples.
pub fn reset() {
    set_sysfs_root(None);
    let local = LOCAL.with(|slot| slot.borrow_mut().take());
    drop(local);
}

pub fn open_source(id: u64, source: Source, frequency_hz: f64) -> OpenResult {
    match mode() {
        Mode::HostOnly => return OpenResult::HostOnly,
        Mode::Unavailable => {
            return OpenResult::Missing("this platform has no live sensor adapter");
        }
        Mode::Files => {}
    }
    let root = current_root();
    let allow_proxy = allow_proxy();
    let snap = load_snapshot(&root, allow_proxy);
    if !source_supported(source, &snap) {
        return OpenResult::Missing("sensor is not exposed by the host");
    }
    with_local(|local| {
        ensure_time_origin(&local.shared);
        {
            let mut subs = local.shared.subs.lock().expect("sensor subs");
            subs.retain(|sub| sub.id != id);
            subs.push(Sub {
                id,
                source,
                interval: period(frequency_hz),
                next_due: Instant::now(),
                root,
                allow_proxy,
            });
        }
        ensure_thread(local);
        local.shared.wake.notify_all();
    });
    OpenResult::Live
}

pub fn close_source(id: u64) {
    let local = LOCAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|local| Arc::clone(&local.shared))
    });
    let Some(shared) = local else {
        return;
    };
    if let Ok(mut subs) = shared.subs.lock() {
        subs.retain(|sub| sub.id != id);
    }
    shared.wake.notify_all();
}

pub fn set_window_interest(orientation: bool, motion: bool) {
    if !matches!(mode(), Mode::Files) {
        close_source(WINDOW_ORIENTATION_ID);
        close_source(WINDOW_MOTION_ID);
        return;
    }
    let root = current_root();
    let allow_proxy = allow_proxy();
    let snap = load_snapshot(&root, allow_proxy);
    with_local(|local| {
        let interval = period(60.0);
        let now = Instant::now();
        {
            let mut subs = local.shared.subs.lock().expect("sensor subs");
            subs.retain(|sub| sub.id != WINDOW_ORIENTATION_ID && sub.id != WINDOW_MOTION_ID);
            if orientation && source_supported(Source::WindowOrientation, &snap) {
                subs.push(Sub {
                    id: WINDOW_ORIENTATION_ID,
                    source: Source::WindowOrientation,
                    interval,
                    next_due: now,
                    root: root.clone(),
                    allow_proxy,
                });
            }
            if motion && source_supported(Source::WindowMotion, &snap) {
                subs.push(Sub {
                    id: WINDOW_MOTION_ID,
                    source: Source::WindowMotion,
                    interval,
                    next_due: now,
                    root,
                    allow_proxy,
                });
            }
        }
        let any = local
            .shared
            .subs
            .lock()
            .map(|subs| {
                subs.iter()
                    .any(|sub| sub.id == WINDOW_ORIENTATION_ID || sub.id == WINDOW_MOTION_ID)
            })
            .unwrap_or(false);
        if any {
            ensure_time_origin(&local.shared);
            ensure_thread(local);
            local.shared.wake.notify_all();
        }
    });
}

pub fn take_samples() -> Vec<Sample> {
    LOCAL.with(|slot| {
        let slot = slot.borrow();
        let Some(local) = slot.as_ref() else {
            return Vec::new();
        };
        local
            .shared
            .samples
            .lock()
            .map(|mut samples| std::mem::take(&mut *samples))
            .unwrap_or_default()
    })
}

pub fn next_deadline() -> Option<Instant> {
    LOCAL.with(|slot| {
        let slot = slot.borrow();
        let local = slot.as_ref()?;
        let subs = local.shared.subs.lock().ok()?;
        if subs.is_empty() {
            return None;
        }
        let now = Instant::now();
        let wait = subs
            .iter()
            .map(|sub| sub.next_due.saturating_duration_since(now))
            .min()?;
        Some(now + wait)
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    HostOnly,
    Files,
    Unavailable,
}

fn mode() -> Mode {
    if root_override().is_some() {
        Mode::Files
    } else if !live_hardware_enabled() {
        Mode::HostOnly
    } else if linux_desktop() {
        Mode::Files
    } else {
        Mode::Unavailable
    }
}

fn live_hardware_enabled() -> bool {
    !cfg!(test) || std::env::var_os("W3COS_SENSOR_DEVICE_TESTS").is_some()
}

fn linux_desktop() -> bool {
    cfg!(all(target_os = "linux", not(target_env = "ohos")))
}

fn allow_proxy() -> bool {
    root_override().is_none() && live_hardware_enabled() && linux_desktop()
}

fn root_override() -> Option<PathBuf> {
    ROOT.with(|root| root.borrow().clone())
}

fn current_root() -> PathBuf {
    root_override().unwrap_or_else(|| PathBuf::from("/sys/bus/iio/devices"))
}

fn period(hz: f64) -> Duration {
    let hz = if hz.is_finite() {
        hz.clamp(MIN_HZ, MAX_HZ)
    } else {
        60.0
    };
    Duration::from_secs_f64(1.0 / hz)
}

fn with_local(f: impl FnOnce(&mut Local)) {
    LOCAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(Local {
                shared: Shared::new(),
                join: None,
            });
        }
        if let Some(local) = slot.as_mut() {
            f(local);
        }
    });
}

fn ensure_time_origin(shared: &Shared) {
    let mut origin = shared.time_origin.lock().expect("sensor clock");
    if origin.is_none() {
        *origin = Some((Instant::now(), crate::jsdom::performance_now()));
    }
}

fn timestamp_ms(shared: &Shared) -> f64 {
    shared
        .time_origin
        .lock()
        .ok()
        .and_then(|origin| {
            origin.map(|(start, perf)| perf + start.elapsed().as_secs_f64() * 1000.0)
        })
        .unwrap_or(0.0)
}

fn ensure_thread(local: &mut Local) {
    if local.shared.running.load(Ordering::SeqCst) {
        return;
    }
    if let Some(previous) = local.join.take() {
        local.shared.stop.store(true, Ordering::SeqCst);
        local.shared.wake.notify_all();
        let _ = previous.join();
    }
    local.shared.stop.store(false, Ordering::SeqCst);
    local.shared.running.store(true, Ordering::SeqCst);
    let shared = Arc::clone(&local.shared);
    match std::thread::Builder::new()
        .name("w3cos-sensors".into())
        .spawn(move || sampler_loop(shared))
    {
        Ok(handle) => local.join = Some(handle),
        Err(err) => {
            local.shared.running.store(false, Ordering::SeqCst);
            eprintln!("[w3cos] warning: sensor sampler failed to start: {err}");
        }
    }
}

fn sampler_loop(shared: Arc<Shared>) {
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let due = take_due(&shared);
        if due.is_empty() {
            match next_wait(&shared) {
                None => {
                    if shared.stop.load(Ordering::SeqCst)
                        || shared
                            .subs
                            .lock()
                            .map(|subs| subs.is_empty())
                            .unwrap_or(true)
                    {
                        break;
                    }
                    continue;
                }
                Some(wait) => {
                    let guard = shared.wake_mutex.lock().expect("sensor wake");
                    if shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = shared.wake.wait_timeout(guard, wait);
                    continue;
                }
            }
        }
        let samples = produce(&shared, &due);
        if !samples.is_empty() {
            if let Ok(mut queued) = shared.samples.lock() {
                queued.extend(samples);
                if queued.len() > 64 {
                    let extra = queued.len() - 64;
                    queued.drain(0..extra);
                }
            }
            crate::uitest::poke_event_loop();
        }
    }
    shared.running.store(false, Ordering::SeqCst);
}

fn take_due(shared: &Shared) -> Vec<Sub> {
    let Ok(mut subs) = shared.subs.lock() else {
        return Vec::new();
    };
    let now = Instant::now();
    let mut due = Vec::new();
    for sub in subs.iter_mut() {
        if sub.next_due <= now {
            due.push(sub.clone());
            sub.next_due = now + sub.interval;
        }
    }
    due
}

fn next_wait(shared: &Shared) -> Option<Duration> {
    let subs = shared.subs.lock().ok()?;
    if subs.is_empty() {
        return None;
    }
    let now = Instant::now();
    Some(
        subs.iter()
            .map(|sub| sub.next_due.saturating_duration_since(now))
            .min()
            .unwrap_or(Duration::from_millis(16)),
    )
}

fn produce(shared: &Shared, due: &[Sub]) -> Vec<Sample> {
    let mut roots: Vec<(PathBuf, bool)> = Vec::new();
    for sub in due {
        if !roots
            .iter()
            .any(|(root, proxy)| root == &sub.root && *proxy == sub.allow_proxy)
        {
            roots.push((sub.root.clone(), sub.allow_proxy));
        }
    }
    let mut out = Vec::new();
    for (root, allow_proxy) in roots {
        let group: Vec<&Sub> = due
            .iter()
            .filter(|sub| sub.root == root && sub.allow_proxy == allow_proxy)
            .collect();
        out.extend(sample_group(shared, &root, allow_proxy, &group));
    }
    out
}

fn sample_group(shared: &Shared, root: &Path, allow_proxy: bool, group: &[&Sub]) -> Vec<Sample> {
    let snap = load_snapshot(root, allow_proxy);
    let mut fusion = shared.fusion.lock().expect("sensor fusion");
    let now = Instant::now();
    let dt = fusion
        .last
        .map(|last| now.saturating_duration_since(last).as_secs_f64())
        .unwrap_or(0.0);
    fusion.last = Some(now);
    if let Some(explicit) = snap.gravity {
        fusion.gravity = Some(explicit);
    } else if let Some(accel) = snap.accel
        && !snap.accel_from_proxy
    {
        fusion.gravity = Some(blend_gravity(fusion.gravity, accel, dt));
    }
    let gravity_vec = snap
        .gravity
        .or(fusion.gravity)
        .or(snap.accel.filter(|_| snap.accel_from_proxy));
    let timestamp = timestamp_ms(shared);
    let mut seen = Vec::new();
    let mut out = Vec::new();
    let mut lost = Vec::new();
    for sub in group {
        if seen.contains(&sub.source) {
            continue;
        }
        seen.push(sub.source);
        if !source_supported(sub.source, &snap) {
            lost.push(sub.source);
            if !is_window(sub.source) {
                out.push(Sample::Unavailable { source: sub.source });
            }
            continue;
        }
        if let Some(sample) = sample_source(sub, &snap, gravity_vec, timestamp) {
            out.push(sample);
        }
    }
    drop(fusion);
    if !lost.is_empty()
        && let Ok(mut subs) = shared.subs.lock()
    {
        subs.retain(|sub| !lost.contains(&sub.source) || sub.root != root);
    }
    out
}

fn is_window(source: Source) -> bool {
    matches!(source, Source::WindowOrientation | Source::WindowMotion)
}

fn sample_source(
    sub: &Sub,
    snap: &Snapshot,
    gravity_vec: Option<[f64; 3]>,
    timestamp: f64,
) -> Option<Sample> {
    let up = gravity_vec.or(snap.accel);
    match sub.source {
        Source::Accelerometer => snap.accel.map(|[x, y, z]| Sample::Vector {
            source: sub.source,
            x,
            y,
            z,
            timestamp,
        }),
        Source::Gravity => gravity_vec
            .filter(|_| snap.gravity.is_some() || snap.accel.is_some())
            .map(|[x, y, z]| Sample::Vector {
                source: sub.source,
                x,
                y,
                z,
                timestamp,
            }),
        Source::LinearAcceleration => {
            let accel = snap.accel.filter(|_| !snap.accel_from_proxy)?;
            let gravity = gravity_vec?;
            let linear = sub_vec(accel, gravity);
            Some(Sample::Vector {
                source: sub.source,
                x: linear[0],
                y: linear[1],
                z: linear[2],
                timestamp,
            })
        }
        Source::Gyroscope => snap.gyro.map(|[x, y, z]| Sample::Vector {
            source: sub.source,
            x,
            y,
            z,
            timestamp,
        }),
        Source::Magnetometer => snap.mag.map(|[x, y, z]| Sample::Vector {
            source: sub.source,
            x,
            y,
            z,
            timestamp,
        }),
        Source::AmbientLight => snap
            .illuminance
            .map(|lux| Sample::Illuminance { lux, timestamp }),
        Source::RelativeOrientation => {
            let quaternion = orientation_quaternion(up?, None)?;
            Some(Sample::Quaternion {
                source: sub.source,
                quaternion,
                timestamp,
            })
        }
        Source::AbsoluteOrientation => {
            let quaternion = orientation_quaternion(up?, snap.mag)?;
            Some(Sample::Quaternion {
                source: sub.source,
                quaternion,
                timestamp,
            })
        }
        Source::WindowMotion => {
            let rotation = snap.gyro.map(|gyro| {
                (
                    gyro[2].to_degrees(),
                    gyro[0].to_degrees(),
                    gyro[1].to_degrees(),
                )
            });
            let linear = match (snap.accel, gravity_vec) {
                (Some(accel), Some(gravity)) if !snap.accel_from_proxy => {
                    Some(sub_vec(accel, gravity))
                }
                _ => None,
            };
            Some(Sample::Motion {
                acceleration: linear,
                including_gravity: snap.accel,
                rotation_alpha: rotation.map(|value| value.0),
                rotation_beta: rotation.map(|value| value.1),
                rotation_gamma: rotation.map(|value| value.2),
                interval_ms: sub.interval.as_secs_f64() * 1000.0,
            })
        }
        Source::WindowOrientation => {
            let up = up?;
            let (beta, gamma) = tilt_degrees(up)?;
            let alpha = snap.mag.and_then(|mag| heading_degrees(up, mag));
            Some(Sample::Orientation {
                alpha,
                beta,
                gamma,
                absolute: alpha.is_some(),
            })
        }
    }
}

fn source_supported(source: Source, snap: &Snapshot) -> bool {
    match source {
        Source::Accelerometer => snap.accel.is_some(),
        Source::Gravity => snap.gravity.is_some() || snap.accel.is_some(),
        Source::LinearAcceleration => snap.accel.is_some() && !snap.accel_from_proxy,
        Source::Gyroscope => snap.gyro.is_some(),
        Source::Magnetometer => snap.mag.is_some(),
        Source::AmbientLight => snap.illuminance.is_some(),
        Source::AbsoluteOrientation => snap.accel.is_some() && snap.mag.is_some(),
        Source::RelativeOrientation | Source::WindowOrientation => {
            snap.accel.is_some() || snap.gravity.is_some()
        }
        Source::WindowMotion => {
            snap.accel.is_some() || snap.gyro.is_some() || snap.gravity.is_some()
        }
    }
}

fn load_snapshot(root: &Path, allow_proxy: bool) -> Snapshot {
    let mut snap = read_sysfs(root);
    if allow_proxy && (snap.accel.is_none() || snap.illuminance.is_none()) {
        if let Some(proxy) = proxy_cached() {
            if snap.accel.is_none()
                && let Some(orientation) = proxy.orientation.as_deref()
                && let Some(gravity) = gravity_from_proxy_orientation(orientation)
            {
                snap.accel = Some(gravity);
                snap.accel_from_proxy = true;
            }
            if snap.illuminance.is_none() {
                snap.illuminance = proxy.lux;
            }
        }
    }
    snap
}

fn read_sysfs(root: &Path) -> Snapshot {
    let mut snap = Snapshot::default();
    absorb(&mut snap, root);
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            absorb(&mut snap, &entry.path());
        }
    }
    snap
}

fn absorb(snap: &mut Snapshot, dir: &Path) {
    if snap.accel.is_none() {
        snap.accel = read_vec(dir, "accel");
    }
    if snap.gravity.is_none() {
        snap.gravity = read_vec(dir, "gravity");
    }
    if snap.gyro.is_none() {
        snap.gyro = read_vec(dir, "anglvel");
    }
    if snap.mag.is_none() {
        snap.mag = read_vec(dir, "magn").map(|value| value.map(|component| component * 100.0));
    }
    if snap.illuminance.is_none() {
        snap.illuminance = read_scalar(dir, "illuminance");
    }
}

fn read_vec(dir: &Path, kind: &str) -> Option<[f64; 3]> {
    let value = [
        read_axis(dir, kind, "x")?,
        read_axis(dir, kind, "y")?,
        read_axis(dir, kind, "z")?,
    ];
    Some(apply_mount(dir, kind, value))
}

fn read_axis(dir: &Path, kind: &str, axis: &str) -> Option<f64> {
    if let Some(raw) = read_f64(&dir.join(format!("in_{kind}_{axis}_raw"))) {
        let scale = read_f64(&dir.join(format!("in_{kind}_{axis}_scale")))
            .or_else(|| read_f64(&dir.join(format!("in_{kind}_scale"))))
            .unwrap_or(1.0);
        let offset = read_f64(&dir.join(format!("in_{kind}_{axis}_offset")))
            .or_else(|| read_f64(&dir.join(format!("in_{kind}_offset"))))
            .unwrap_or(0.0);
        let value = (raw + offset) * scale;
        return value.is_finite().then_some(value);
    }
    read_f64(&dir.join(format!("in_{kind}_{axis}")))
        .or_else(|| read_f64(&dir.join(format!("in_{kind}_{axis}_input"))))
}

fn read_scalar(dir: &Path, kind: &str) -> Option<f64> {
    if let Some(raw) = read_f64(&dir.join(format!("in_{kind}_raw"))) {
        let scale = read_f64(&dir.join(format!("in_{kind}_scale"))).unwrap_or(1.0);
        let offset = read_f64(&dir.join(format!("in_{kind}_offset"))).unwrap_or(0.0);
        let value = (raw + offset) * scale;
        return value.is_finite().then_some(value);
    }
    read_f64(&dir.join(format!("in_{kind}")))
}

fn apply_mount(dir: &Path, kind: &str, value: [f64; 3]) -> [f64; 3] {
    let matrix = read_matrix(&dir.join(format!("in_{kind}_mount_matrix")))
        .or_else(|| read_matrix(&dir.join("in_mount_matrix")))
        .or_else(|| read_matrix(&dir.join("mount_matrix")));
    match matrix {
        Some(matrix) => mul_matrix(matrix, value),
        None => value,
    }
}

fn read_matrix(path: &Path) -> Option<[[f64; 3]; 3]> {
    let text = fs::read_to_string(path).ok()?;
    let nums: Vec<f64> = text
        .split(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E')))
        .filter(|token| !token.is_empty())
        .filter_map(|token| token.parse().ok())
        .collect();
    if nums.len() < 9 {
        return None;
    }
    Some([
        [nums[0], nums[1], nums[2]],
        [nums[3], nums[4], nums[5]],
        [nums[6], nums[7], nums[8]],
    ])
}

fn read_f64(path: &Path) -> Option<f64> {
    let text = fs::read_to_string(path).ok()?;
    let value = text.split_whitespace().next()?.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

fn blend_gravity(prev: Option<[f64; 3]>, accel: [f64; 3], dt: f64) -> [f64; 3] {
    let Some(prev) = prev else {
        return accel;
    };
    let alpha = if dt <= 0.0 { 0.0 } else { dt / (0.25 + dt) };
    lerp(prev, accel, alpha)
}

/// iio-sensor-proxy reports which screen edge is up, not a full acceleration
/// vector. The result is the at-rest specific force for that pose.
fn gravity_from_proxy_orientation(orientation: &str) -> Option<[f64; 3]> {
    match orientation {
        "normal" => Some([0.0, G, 0.0]),
        "bottom-up" => Some([0.0, -G, 0.0]),
        "left-up" => Some([-G, 0.0, 0.0]),
        "right-up" => Some([G, 0.0, 0.0]),
        _ => None,
    }
}

fn proxy_cached() -> Option<ProxyReading> {
    let Ok(mut cache) = proxy_slot().lock() else {
        return None;
    };
    if let Some((when, reading)) = cache.as_ref()
        && when.elapsed() < Duration::from_millis(500)
    {
        return reading.clone().present();
    }
    let reading = read_sensor_proxy().unwrap_or_default();
    *cache = Some((Instant::now(), reading.clone()));
    reading.present()
}

impl ProxyReading {
    fn present(self) -> Option<Self> {
        if self.orientation.is_none() && self.lux.is_none() {
            None
        } else {
            Some(self)
        }
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn scale_vec(a: [f64; 3], scale: f64) -> [f64; 3] {
    a.map(|component| component * scale)
}

fn add_vec(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub_vec(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn normalize(a: [f64; 3]) -> Option<[f64; 3]> {
    let length = norm(a);
    if !length.is_finite() || length < 1e-8 {
        None
    } else {
        Some(scale_vec(a, 1.0 / length))
    }
}

fn reject(value: [f64; 3], axis: [f64; 3]) -> [f64; 3] {
    sub_vec(value, scale_vec(axis, dot(value, axis)))
}

fn lerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    add_vec(scale_vec(a, 1.0 - t), scale_vec(b, t))
}

fn mul_matrix(matrix: [[f64; 3]; 3], value: [f64; 3]) -> [f64; 3] {
    [
        dot(matrix[0], value),
        dot(matrix[1], value),
        dot(matrix[2], value),
    ]
}

fn fallback_north(up: [f64; 3]) -> Option<[f64; 3]> {
    let y = reject([0.0, 1.0, 0.0], up);
    if norm(y) > 0.2 {
        return normalize(y);
    }
    normalize(reject([0.0, 0.0, 1.0], up))
}

fn device_to_earth(up_raw: [f64; 3], mag: Option<[f64; 3]>) -> Option<[[f64; 3]; 3]> {
    let up = normalize(up_raw)?;
    let north = if let Some(mag) = mag {
        let horizontal = reject(mag, up);
        if norm(horizontal) >= 1e-6 {
            normalize(horizontal)?
        } else {
            fallback_north(up)?
        }
    } else {
        fallback_north(up)?
    };
    let east = normalize(cross(north, up))?;
    let north = normalize(cross(up, east))?;
    Some([east, north, up])
}

fn matrix_to_quat(m: [[f64; 3]; 3]) -> [f64; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let (x, y, z, w) = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        (
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            0.25 * s,
        )
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        (
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        )
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        (
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        )
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        (
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        )
    };
    let q = [x, y, z, w];
    let length = q
        .iter()
        .map(|component| component * component)
        .sum::<f64>()
        .sqrt();
    if !length.is_finite() || length == 0.0 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        q.map(|component| component / length)
    }
}

fn orientation_quaternion(up: [f64; 3], mag: Option<[f64; 3]>) -> Option<[f64; 4]> {
    Some(matrix_to_quat(device_to_earth(up, mag)?))
}

fn wrap360(degrees: f64) -> f64 {
    let mut wrapped = degrees % 360.0;
    if wrapped < 0.0 {
        wrapped += 360.0;
    }
    wrapped
}

fn heading_degrees(up_raw: [f64; 3], mag: [f64; 3]) -> Option<f64> {
    let up = normalize(up_raw)?;
    let north = normalize(reject(mag, up))?;
    let facing = fallback_north(up)?;
    let sin = dot(up, cross(north, facing));
    let cos = dot(north, facing);
    if !sin.is_finite() || !cos.is_finite() {
        return None;
    }
    Some(wrap360(sin.atan2(cos).to_degrees()))
}

fn tilt_degrees(up_raw: [f64; 3]) -> Option<(f64, f64)> {
    let [x, y, z] = normalize(up_raw)?;
    let beta = y.atan2(z).to_degrees();
    let gamma = (-x).atan2((y * y + z * z).sqrt()).to_degrees();
    (beta.is_finite() && gamma.is_finite()).then_some((beta, gamma))
}

fn read_sensor_proxy() -> Option<ProxyReading> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        dbus::read_sensor_proxy()
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        None
    }
}

mod dbus {
    use super::ProxyReading;

    const CALL: u8 = 1;
    const RETURN: u8 = 2;
    const PATH: u8 = 1;
    const INTERFACE: u8 = 2;
    const MEMBER: u8 = 3;
    const REPLY_SERIAL: u8 = 5;
    const DESTINATION: u8 = 6;
    const SIGNATURE: u8 = 8;

    struct Writer {
        buf: Vec<u8>,
        base: usize,
    }

    impl Writer {
        fn new(base: usize) -> Self {
            Self {
                buf: Vec::new(),
                base,
            }
        }

        fn align(&mut self, alignment: usize) {
            let abs = self.base + self.buf.len();
            let rem = abs % alignment;
            if rem != 0 {
                self.buf.resize(self.buf.len() + (alignment - rem), 0);
            }
        }

        fn u8(&mut self, value: u8) {
            self.buf.push(value);
        }

        fn u32(&mut self, value: u32) {
            self.align(4);
            self.buf.extend_from_slice(&value.to_le_bytes());
        }

        fn f64(&mut self, value: f64) {
            self.align(8);
            self.buf.extend_from_slice(&value.to_le_bytes());
        }

        fn string(&mut self, value: &str) {
            self.align(4);
            let bytes = value.as_bytes();
            self.buf
                .extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            self.buf.extend_from_slice(bytes);
            self.buf.push(0);
        }

        fn signature(&mut self, value: &str) {
            self.buf.push(value.len() as u8);
            self.buf.extend_from_slice(value.as_bytes());
            self.buf.push(0);
        }
    }

    struct Reader<'a> {
        data: &'a [u8],
        pos: usize,
        base: usize,
    }

    impl<'a> Reader<'a> {
        fn new(data: &'a [u8], base: usize) -> Self {
            Self { data, pos: 0, base }
        }

        fn align(&mut self, alignment: usize) -> Option<()> {
            let abs = self.base + self.pos;
            let rem = abs % alignment;
            if rem != 0 {
                self.pos += alignment - rem;
            }
            (self.pos <= self.data.len()).then_some(())
        }

        fn u8(&mut self) -> Option<u8> {
            let value = *self.data.get(self.pos)?;
            self.pos += 1;
            Some(value)
        }

        fn bytes(&mut self, count: usize) -> Option<&'a [u8]> {
            let end = self.pos.checked_add(count)?;
            let value = self.data.get(self.pos..end)?;
            self.pos = end;
            Some(value)
        }

        fn u32(&mut self) -> Option<u32> {
            self.align(4)?;
            let bytes = self.bytes(4)?;
            Some(u32::from_le_bytes(bytes.try_into().ok()?))
        }

        fn f64(&mut self) -> Option<f64> {
            self.align(8)?;
            let bytes = self.bytes(8)?;
            Some(f64::from_le_bytes(bytes.try_into().ok()?))
        }

        fn string(&mut self) -> Option<String> {
            self.align(4)?;
            let len = self.u32()? as usize;
            let bytes = self.bytes(len)?;
            if self.u8()? != 0 {
                return None;
            }
            Some(String::from_utf8(bytes.to_vec()).ok()?)
        }

        fn signature(&mut self) -> Option<String> {
            let len = self.u8()? as usize;
            let bytes = self.bytes(len)?;
            if self.u8()? != 0 {
                return None;
            }
            Some(String::from_utf8(bytes.to_vec()).ok()?)
        }

        fn value(&mut self, signature: &str) -> Option<DbusValue> {
            match signature {
                "s" | "o" => Some(DbusValue::String(self.string()?)),
                "g" => Some(DbusValue::String(self.signature()?)),
                "u" => Some(DbusValue::U32(self.u32()?)),
                "b" => Some(DbusValue::Bool(self.u32()? != 0)),
                "d" => Some(DbusValue::F64(self.f64()?)),
                "v" => {
                    let inner = self.signature()?;
                    Some(self.value(&inner)?)
                }
                "" => Some(DbusValue::Unit),
                _ => None,
            }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum DbusValue {
        Unit,
        String(String),
        U32(u32),
        Bool(bool),
        F64(f64),
    }

    struct Message {
        kind: u8,
        serial: u32,
        reply_serial: Option<u32>,
        destination: Option<String>,
        path: Option<String>,
        interface: Option<String>,
        member: Option<String>,
        signature: Option<String>,
        body: Vec<u8>,
    }

    impl Message {
        fn call(destination: &str, path: &str, interface: &str, member: &str) -> Self {
            Self {
                kind: CALL,
                serial: 0,
                reply_serial: None,
                destination: Some(destination.into()),
                path: Some(path.into()),
                interface: Some(interface.into()),
                member: Some(member.into()),
                signature: None,
                body: Vec::new(),
            }
        }

        fn encode(&self) -> Vec<u8> {
            let mut fields = Writer::new(16);
            if let Some(value) = &self.path {
                write_field(&mut fields, PATH, "o", value);
            }
            if let Some(value) = &self.interface {
                write_field(&mut fields, INTERFACE, "s", value);
            }
            if let Some(value) = &self.member {
                write_field(&mut fields, MEMBER, "s", value);
            }
            if let Some(value) = self.reply_serial {
                fields.align(8);
                fields.u8(REPLY_SERIAL);
                fields.signature("u");
                fields.u32(value);
            }
            if let Some(value) = &self.destination {
                write_field(&mut fields, DESTINATION, "s", value);
            }
            if let Some(value) = &self.signature {
                fields.align(8);
                fields.u8(SIGNATURE);
                fields.signature("g");
                fields.signature(value);
            }
            let mut out = Writer::new(0);
            out.u8(b'l');
            out.u8(self.kind);
            out.u8(0);
            out.u8(1);
            out.u32(self.body.len() as u32);
            out.u32(self.serial);
            out.u32(fields.buf.len() as u32);
            out.buf.extend_from_slice(&fields.buf);
            out.align(8);
            out.buf.extend_from_slice(&self.body);
            out.buf
        }
    }

    fn write_field(writer: &mut Writer, code: u8, signature: &str, value: &str) {
        writer.align(8);
        writer.u8(code);
        writer.signature(signature);
        writer.string(value);
    }

    fn decode_message(bytes: &[u8]) -> Option<Message> {
        if bytes.len() < 16 || bytes[0] != b'l' {
            return None;
        }
        let body_len = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let serial = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let fields_len = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
        let fields_end = 16usize.checked_add(fields_len)?;
        if fields_end > bytes.len() || body_len > 1_000_000 || fields_len > 1_000_000 {
            return None;
        }
        let mut reader = Reader::new(&bytes[16..fields_end], 16);
        let mut message = Message {
            kind: bytes[1],
            serial,
            reply_serial: None,
            destination: None,
            path: None,
            interface: None,
            member: None,
            signature: None,
            body: Vec::new(),
        };
        while reader.pos < reader.data.len() {
            reader.align(8)?;
            if reader.pos >= reader.data.len() {
                break;
            }
            let code = reader.u8()?;
            let signature = reader.signature()?;
            let value = reader.value(&signature)?;
            match (code, value) {
                (PATH, DbusValue::String(value)) => message.path = Some(value),
                (INTERFACE, DbusValue::String(value)) => message.interface = Some(value),
                (MEMBER, DbusValue::String(value)) => message.member = Some(value),
                (REPLY_SERIAL, DbusValue::U32(value)) => message.reply_serial = Some(value),
                (DESTINATION, DbusValue::String(value)) => message.destination = Some(value),
                (SIGNATURE, DbusValue::String(value)) => message.signature = Some(value),
                _ => {}
            }
        }
        let header_end = 16 + fields_len;
        let pad = (8 - header_end % 8) % 8;
        let body_at = header_end + pad;
        let body_end = body_at.checked_add(body_len)?;
        if body_end > bytes.len() {
            return None;
        }
        message.body = bytes[body_at..body_end].to_vec();
        Some(message)
    }

    fn body_value(message: &Message) -> Option<DbusValue> {
        let signature = message.signature.clone().unwrap_or_default();
        let mut reader = Reader::new(&message.body, 0);
        reader.value(&signature)
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    struct Bus {
        stream: std::os::unix::net::UnixStream,
        serial: u32,
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    impl Bus {
        fn connect() -> Option<Self> {
            use std::io::Write;
            let path = system_socket()?;
            let mut stream = std::os::unix::net::UnixStream::connect(path).ok()?;
            stream
                .set_read_timeout(Some(std::time::Duration::from_millis(200)))
                .ok()?;
            stream
                .set_write_timeout(Some(std::time::Duration::from_millis(200)))
                .ok()?;
            let uid = unsafe { libc::geteuid() };
            let hex: String = uid
                .to_string()
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            stream
                .write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())
                .ok()?;
            let line = read_line(&mut stream)?;
            if !line.starts_with("OK") {
                return None;
            }
            stream.write_all(b"BEGIN\r\n").ok()?;
            Some(Self { stream, serial: 0 })
        }

        fn call(&mut self, mut message: Message) -> Option<Message> {
            use std::io::Write;
            self.serial = self.serial.wrapping_add(1);
            message.serial = self.serial;
            self.stream.write_all(&message.encode()).ok()?;
            for _ in 0..8 {
                let reply = read_one(&mut self.stream)?;
                if reply.kind == 4 {
                    continue;
                }
                if reply.reply_serial == Some(message.serial) {
                    return (reply.kind == RETURN).then_some(reply);
                }
            }
            None
        }
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn system_socket() -> Option<std::path::PathBuf> {
        use std::path::{Path, PathBuf};
        if let Ok(address) = std::env::var("DBUS_SYSTEM_BUS_ADDRESS") {
            for part in address.split(',') {
                if let Some(path) = part.trim().strip_prefix("unix:path=") {
                    return Some(PathBuf::from(path));
                }
            }
        }
        for candidate in [
            "/run/dbus/system_bus_socket",
            "/var/run/dbus/system_bus_socket",
        ] {
            if Path::new(candidate).exists() {
                return Some(PathBuf::from(candidate));
            }
        }
        None
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn read_line(stream: &mut std::os::unix::net::UnixStream) -> Option<String> {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        for _ in 0..512 {
            stream.read_exact(&mut byte).ok()?;
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        Some(String::from_utf8_lossy(&buf).trim().to_string())
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn read_one(stream: &mut std::os::unix::net::UnixStream) -> Option<Message> {
        use std::io::Read;
        let mut fixed = [0u8; 16];
        stream.read_exact(&mut fixed).ok()?;
        if fixed[0] != b'l' {
            return None;
        }
        let body_len = u32::from_le_bytes(fixed[4..8].try_into().ok()?) as usize;
        let fields_len = u32::from_le_bytes(fixed[12..16].try_into().ok()?) as usize;
        if body_len > 1_000_000 || fields_len > 1_000_000 {
            return None;
        }
        let mut fields = vec![0u8; fields_len];
        stream.read_exact(&mut fields).ok()?;
        let header_end = 16 + fields_len;
        let pad = (8 - header_end % 8) % 8;
        if pad > 0 {
            let mut padding = vec![0u8; pad];
            stream.read_exact(&mut padding).ok()?;
        }
        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            stream.read_exact(&mut body).ok()?;
        }
        let mut bytes = Vec::with_capacity(header_end + pad + body_len);
        bytes.extend_from_slice(&fixed);
        bytes.extend_from_slice(&fields);
        bytes.resize(header_end + pad, 0);
        bytes.extend_from_slice(&body);
        decode_message(&bytes)
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    pub(super) fn read_sensor_proxy() -> Option<ProxyReading> {
        let mut bus = Bus::connect()?;
        let hello = Message::call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
        );
        bus.call(hello)?;
        let _ = bus.call(proxy_call("ClaimAccelerometer", None));
        let _ = bus.call(proxy_call("ClaimLight", None));
        let mut reading = ProxyReading::default();
        if proxy_bool(&mut bus, "HasAccelerometer") == Some(true) {
            reading.orientation = proxy_string(&mut bus, "AccelerometerOrientation");
        }
        let unit = proxy_string(&mut bus, "LightLevelUnit");
        let lux_ok = !matches!(unit.as_deref(), Some("vendor"));
        if lux_ok && proxy_bool(&mut bus, "HasAmbientLight") != Some(false) {
            reading.lux = proxy_f64(&mut bus, "LightLevel").filter(|value| value.is_finite());
        }
        reading.present()
    }

    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    pub(super) fn read_sensor_proxy() -> Option<ProxyReading> {
        None
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn proxy_call(member: &str, body: Option<(&str, Vec<u8>)>) -> Message {
        let mut message = Message::call(
            "net.hadess.SensorProxy",
            "/net/hadess/SensorProxy",
            if body.is_some() {
                "org.freedesktop.DBus.Properties"
            } else {
                "net.hadess.SensorProxy"
            },
            member,
        );
        if let Some((signature, bytes)) = body {
            message.signature = Some(signature.into());
            message.body = bytes;
        }
        message
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn proxy_get(bus: &mut Bus, property: &str) -> Option<DbusValue> {
        let mut body = Writer::new(0);
        body.string("net.hadess.SensorProxy");
        body.string(property);
        let message = proxy_call("Get", Some(("ss", body.buf)));
        body_value(&bus.call(message)?)
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn proxy_bool(bus: &mut Bus, property: &str) -> Option<bool> {
        match proxy_get(bus, property)? {
            DbusValue::Bool(value) => Some(value),
            _ => None,
        }
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn proxy_string(bus: &mut Bus, property: &str) -> Option<String> {
        match proxy_get(bus, property)? {
            DbusValue::String(value) => Some(value),
            _ => None,
        }
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn proxy_f64(bus: &mut Bus, property: &str) -> Option<f64> {
        match proxy_get(bus, property)? {
            DbusValue::F64(value) => Some(value),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hello_call_roundtrips_header_fields() {
            let mut message = Message::call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "Hello",
            );
            message.serial = 7;
            let decoded = decode_message(&message.encode()).expect("hello");
            assert_eq!(decoded.kind, CALL);
            assert_eq!(decoded.serial, 7);
            assert_eq!(decoded.member.as_deref(), Some("Hello"));
            assert_eq!(decoded.path.as_deref(), Some("/org/freedesktop/DBus"));
            assert_eq!(decoded.destination.as_deref(), Some("org.freedesktop.DBus"));
            assert!(decoded.body.is_empty());
        }

        #[test]
        fn variant_double_and_strings_roundtrip() {
            let mut body = Writer::new(0);
            body.signature("d");
            body.f64(12.5);
            let message = Message {
                kind: RETURN,
                serial: 3,
                reply_serial: Some(7),
                destination: None,
                path: None,
                interface: None,
                member: None,
                signature: Some("v".into()),
                body: body.buf,
            };
            let decoded = decode_message(&message.encode()).expect("variant");
            assert_eq!(decoded.reply_serial, Some(7));
            assert_eq!(body_value(&decoded), Some(DbusValue::F64(12.5)));

            let mut strings = Writer::new(0);
            strings.string("net.hadess.SensorProxy");
            strings.string("LightLevel");
            let get = Message {
                kind: CALL,
                serial: 4,
                reply_serial: None,
                destination: Some("net.hadess.SensorProxy".into()),
                path: Some("/net/hadess/SensorProxy".into()),
                interface: Some("org.freedesktop.DBus.Properties".into()),
                member: Some("Get".into()),
                signature: Some("ss".into()),
                body: strings.buf,
            };
            let decoded = decode_message(&get.encode()).expect("get");
            let mut reader = Reader::new(&decoded.body, 0);
            assert_eq!(reader.string().as_deref(), Some("net.hadess.SensorProxy"));
            assert_eq!(reader.string().as_deref(), Some("LightLevel"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_device(root: &Path, name: &str, files: &[(&str, &str)]) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            fs::write(dir.join(name), contents).unwrap();
        }
    }

    #[test]
    fn scale_offset_mount_matrix_and_separate_devices() {
        let root = std::env::temp_dir().join(format!(
            "w3cos-iio-parse-{}-{}",
            std::process::id(),
            js_nonce()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        write_device(
            &root,
            "iio:device0",
            &[
                ("in_accel_x_raw", "10\n"),
                ("in_accel_y_raw", "0\n"),
                ("in_accel_z_raw", "981\n"),
                ("in_accel_scale", "0.01\n"),
                ("in_accel_x_offset", "5\n"),
                ("in_mount_matrix", "0, 1, 0; 1, 0, 0; 0, 0, 1\n"),
                ("in_gravity_x_raw", "0\n"),
                ("in_gravity_y_raw", "1\n"),
                ("in_gravity_z_raw", "0\n"),
                ("in_gravity_scale", "9.81\n"),
            ],
        );
        write_device(
            &root,
            "iio:device1",
            &[
                ("in_anglvel_x_raw", "0\n"),
                ("in_anglvel_y_raw", "0\n"),
                ("in_anglvel_z_raw", "100\n"),
                ("in_anglvel_scale", "0.01\n"),
                ("in_magn_x_raw", "1\n"),
                ("in_magn_y_raw", "0\n"),
                ("in_magn_z_raw", "0\n"),
                ("in_magn_scale", "0.5\n"),
                ("in_illuminance_raw", "25\n"),
                ("in_illuminance_scale", "2\n"),
                ("in_illuminance_offset", "1\n"),
            ],
        );
        let snap = read_sysfs(&root);
        let accel = snap.accel.expect("accel");
        assert!((accel[0] - 0.0).abs() < 1e-9);
        assert!((accel[1] - 0.15).abs() < 1e-9);
        assert!((accel[2] - 9.81).abs() < 1e-9);
        // The device mount matrix swaps X and Y for every channel on device0.
        assert_eq!(snap.gravity, Some([9.81, 0.0, 0.0]));
        assert!((snap.gyro.expect("gyro")[2] - 1.0).abs() < 1e-9);
        assert!((snap.mag.expect("mag")[0] - 50.0).abs() < 1e-9);
        assert!((snap.illuminance.expect("lux") - 52.0).abs() < 1e-9);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn orientation_math_matches_flat_and_cardinal_poses() {
        let identity = orientation_quaternion([0.0, 0.0, 9.81], Some([0.0, 0.4, 0.0])).unwrap();
        assert!(identity[3] > 0.99, "{identity:?}");
        assert!(identity[0].abs() < 0.02 && identity[1].abs() < 0.02 && identity[2].abs() < 0.02);
        let (beta, gamma) = tilt_degrees([0.0, 9.81, 0.0]).unwrap();
        assert!((beta - 90.0).abs() < 0.01);
        assert!(gamma.abs() < 0.01);
        let (beta, gamma) = tilt_degrees([0.0, 0.0, 9.81]).unwrap();
        assert!(beta.abs() < 0.01 && gamma.abs() < 0.01);
        let east = heading_degrees([0.0, 0.0, 1.0], [-1.0, 0.0, 0.0]).unwrap();
        assert!((east - 270.0).abs() < 0.01, "{east}");
        let north = heading_degrees([0.0, 0.0, 1.0], [0.0, 2.0, 0.0]).unwrap();
        assert!(north.abs() < 0.01 || (north - 360.0).abs() < 0.01);
        assert_eq!(
            gravity_from_proxy_orientation("left-up"),
            Some([-G, 0.0, 0.0])
        );
        assert!(gravity_from_proxy_orientation("undefined").is_none());
        let first = blend_gravity(None, [0.0, 0.0, 9.81], 0.0);
        let held = blend_gravity(Some(first), [1.0, 0.0, 9.81], 0.0);
        assert_eq!(held, first);
        let moved = blend_gravity(Some(first), [1.0, 0.0, 9.81], 0.25);
        assert!(moved[0] > 0.4 && moved[0] < 0.6);
    }

    fn js_nonce() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn opted_in_hardware_returns_a_finite_channel_or_skips() {
        if std::env::var_os("W3COS_SENSOR_DEVICE_TESTS").is_none() {
            eprintln!("skipping sensor device test: set W3COS_SENSOR_DEVICE_TESTS=1");
            return;
        }
        let snap = read_sysfs(Path::new("/sys/bus/iio/devices"));
        let proxy = read_sensor_proxy();
        let saw_channel = snap.accel.is_some()
            || snap.gyro.is_some()
            || snap.mag.is_some()
            || snap.illuminance.is_some()
            || snap.gravity.is_some()
            || proxy.is_some();
        if !saw_channel {
            eprintln!(
                "W3COS_SENSOR_DEVICE_TESTS=1 but this host has no IIO channel or iio-sensor-proxy reading"
            );
            return;
        }
        assert!(
            snap.accel
                .or(snap.gyro)
                .or(snap.mag)
                .or(snap.gravity)
                .is_none_or(|vector| vector.iter().all(|component| component.is_finite()))
        );
        if let Some(lux) = snap.illuminance {
            assert!(lux.is_finite());
        }
        if let Some(proxy) = proxy {
            if let Some(orientation) = proxy.orientation {
                assert!(
                    gravity_from_proxy_orientation(&orientation).is_some()
                        || orientation == "undefined"
                );
            }
            if let Some(lux) = proxy.lux {
                assert!(lux.is_finite());
            }
        }
    }
}
