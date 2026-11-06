//! Microphone capture for `getUserMedia({audio})` through `cpal`.
//!
//! Desktop builds with `web-media-advanced` open a real input stream on the
//! host's default audio backend (CoreAudio, WASAPI, ALSA). Android, iOS, OHOS,
//! and builds without that feature have no input adapter: enumeration is empty
//! and opening a device fails, so `getUserMedia` rejects instead of pretending
//! a microphone exists.
//!
//! Captured PCM lives in a [`PcmQueue`] the audio renderer reads by cursor.
//! Several `MediaStreamTrack` clones can hear the same samples. The `cpal`
//! stream is not `Send`, so it stays on a dedicated thread.
//!
//! Unit tests never open a device. [`set_microphone_override`] installs a
//! software microphone and [`push_microphone_samples`] feeds it. Hardware
//! capture is opt-in via `W3COS_AUDIO_DEVICE_TESTS=1`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrophoneInfo {
    pub label: String,
    /// Stable host identity; `mediaDevices` hashes it into `deviceId`.
    pub device_key: String,
    /// Shared physical-device identity; hashed into `groupId`.
    pub group_key: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub is_default: bool,
}

/// What `getUserMedia` asked for. A bare or `ideal` value is a preference;
/// `exact`, `min`, and `max` are mandatory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapturePrefs {
    pub channels: Option<u16>,
    pub channels_exact: bool,
    pub channels_min: Option<u16>,
    pub channels_max: Option<u16>,
    pub sample_rate: Option<u32>,
    pub sample_rate_exact: bool,
    pub sample_rate_min: Option<u32>,
    pub sample_rate_max: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicError {
    pub name: &'static str,
    pub message: String,
    pub constraint: Option<&'static str>,
}

impl MicError {
    fn overconstrained(constraint: &'static str, message: impl Into<String>) -> Self {
        Self {
            name: "OverconstrainedError",
            message: message.into(),
            constraint: Some(constraint),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            name: "NotFoundError",
            message: message.into(),
            constraint: None,
        }
    }

    fn not_readable(message: impl Into<String>) -> Self {
        Self {
            name: "NotReadableError",
            message: message.into(),
            constraint: None,
        }
    }
}

/// One supported `(channels, min rate, max rate)` window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RateWindow {
    pub channels: u16,
    pub min_rate: u32,
    pub max_rate: u32,
}

/// Picks the channel count and sample rate a capture should open with.
pub(crate) fn select_format(
    windows: &[RateWindow],
    default_channels: u16,
    default_rate: u32,
    prefs: &CapturePrefs,
) -> Result<(u16, u32), MicError> {
    let channels = select_channels(windows, default_channels, prefs)?;
    let rate = select_rate(windows, channels, default_rate, prefs)?;
    Ok((channels, rate))
}

fn select_channels(
    windows: &[RateWindow],
    default_channels: u16,
    prefs: &CapturePrefs,
) -> Result<u16, MicError> {
    let fits = |channels: u16| {
        windows.iter().any(|window| window.channels == channels)
            && prefs.channels_min.is_none_or(|min| channels >= min)
            && prefs.channels_max.is_none_or(|max| channels <= max)
    };
    if let Some(wanted) = prefs.channels {
        if fits(wanted) {
            return Ok(wanted);
        }
        if prefs.channels_exact {
            return Err(MicError::overconstrained(
                "channelCount",
                "channelCount is not supported by the microphone",
            ));
        }
    }
    if fits(default_channels) {
        return Ok(default_channels);
    }
    if let Some(fallback) = windows
        .iter()
        .map(|window| window.channels)
        .find(|channels| fits(*channels))
    {
        return Ok(fallback);
    }
    if prefs.channels.is_some() || prefs.channels_min.is_some() || prefs.channels_max.is_some() {
        return Err(MicError::overconstrained(
            "channelCount",
            "channelCount is outside the microphone's supported range",
        ));
    }
    Err(MicError::not_readable(
        "the microphone has no supported channel count",
    ))
}

fn select_rate(
    windows: &[RateWindow],
    channels: u16,
    default_rate: u32,
    prefs: &CapturePrefs,
) -> Result<u32, MicError> {
    let allowed = |rate: u32| {
        rate > 0
            && windows.iter().any(|window| {
                window.channels == channels && rate >= window.min_rate && rate <= window.max_rate
            })
            && prefs.sample_rate_min.is_none_or(|min| rate >= min)
            && prefs.sample_rate_max.is_none_or(|max| rate <= max)
    };
    if let Some(wanted) = prefs.sample_rate {
        if allowed(wanted) {
            return Ok(wanted);
        }
        if prefs.sample_rate_exact {
            return Err(MicError::overconstrained(
                "sampleRate",
                "sampleRate is not supported by the microphone",
            ));
        }
    }
    if allowed(default_rate) {
        return Ok(default_rate);
    }
    let fallback = windows
        .iter()
        .filter(|window| window.channels == channels)
        .filter_map(|window| {
            let low = window
                .min_rate
                .max(prefs.sample_rate_min.unwrap_or(window.min_rate));
            let high = window
                .max_rate
                .min(prefs.sample_rate_max.unwrap_or(window.max_rate));
            (low > 0 && low <= high).then_some(high)
        })
        .max();
    if let Some(rate) = fallback {
        return Ok(rate);
    }
    if prefs.sample_rate.is_some()
        || prefs.sample_rate_min.is_some()
        || prefs.sample_rate_max.is_some()
    {
        return Err(MicError::overconstrained(
            "sampleRate",
            "sampleRate is outside the microphone's supported range",
        ));
    }
    Err(MicError::not_readable(
        "the microphone has no supported sample rate",
    ))
}

#[derive(Debug)]
struct QueueInner {
    /// Interleaved frames. The front is the oldest sample still retained.
    samples: std::collections::VecDeque<f32>,
    /// Absolute number of frames ever written.
    written: u64,
}

/// Broadcast PCM buffer. Readers keep their own cursors, so a track and its
/// clone hear the same capture.
#[derive(Debug)]
pub struct PcmQueue {
    inner: Mutex<QueueInner>,
    channels: u16,
    sample_rate: u32,
    cap_frames: usize,
}

impl PcmQueue {
    pub fn new(channels: u16, sample_rate: u32) -> Arc<Self> {
        let channels = channels.max(1);
        let sample_rate = sample_rate.max(1);
        Arc::new(Self {
            inner: Mutex::new(QueueInner {
                samples: std::collections::VecDeque::new(),
                written: 0,
            }),
            channels,
            sample_rate,
            // One second, so a track that is opened before it is connected
            // does not accumulate unbounded latency.
            cap_frames: sample_rate as usize,
        })
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn buffered_frames(&self) -> usize {
        let inner = self.lock();
        inner.samples.len() / self.channels.max(1) as usize
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn push_interleaved(&self, data: &[f32]) {
        let channels = self.channels.max(1) as usize;
        if data.is_empty() || channels == 0 {
            return;
        }
        let usable = data.len() - data.len() % channels;
        if usable == 0 {
            return;
        }
        let mut inner = self.lock();
        inner.samples.extend(data[..usable].iter().copied());
        inner.written += (usable / channels) as u64;
        let max_samples = self.cap_frames.saturating_mul(channels);
        if inner.samples.len() > max_samples {
            let mut drop_n = inner.samples.len() - max_samples;
            drop_n -= drop_n % channels;
            inner.samples.drain(..drop_n);
        }
    }

    /// First frame a new reader should play. A short buffer (a test fixture,
    /// or a capture that just started) is read from the beginning. A capture
    /// that has already filled the cap starts a short way behind the live
    /// edge so connecting a node does not replay a second of history.
    pub fn start_cursor(&self) -> u64 {
        let inner = self.lock();
        let channels = self.channels.max(1) as usize;
        let buffered = (inner.samples.len() / channels) as u64;
        let available = inner.written.saturating_sub(buffered);
        if buffered <= 256 {
            available
        } else {
            inner.written.saturating_sub(256).max(available)
        }
    }

    /// Copies `frames` interleaved frames starting at `cursor` and advances
    /// it. A cursor that fell off the cap skips forward. A cursor at the live
    /// edge returns a short buffer and stays there.
    pub fn read_frames(&self, cursor: &mut u64, frames: usize) -> Vec<f32> {
        let channels = self.channels.max(1) as usize;
        let inner = self.lock();
        let buffered = inner.samples.len() / channels;
        if buffered == 0 || frames == 0 {
            return Vec::new();
        }
        let available = inner.written.saturating_sub(buffered as u64);
        if *cursor < available {
            *cursor = available;
        }
        if *cursor >= inner.written {
            return Vec::new();
        }
        let have = (inner.written - *cursor) as usize;
        let take = frames.min(have);
        let offset = (*cursor - available) as usize * channels;
        let out: Vec<f32> = inner
            .samples
            .iter()
            .skip(offset)
            .take(take * channels)
            .copied()
            .collect();
        *cursor += take as u64;
        out
    }

    /// One frame at an absolute index, without moving a cursor.
    pub fn copy_frame(&self, index: u64) -> Option<Vec<f32>> {
        let channels = self.channels.max(1) as usize;
        let inner = self.lock();
        let buffered = inner.samples.len() / channels;
        if buffered == 0 {
            return None;
        }
        let available = inner.written.saturating_sub(buffered as u64);
        if index < available || index >= inner.written {
            return None;
        }
        let offset = (index - available) as usize * channels;
        Some(
            inner
                .samples
                .iter()
                .skip(offset)
                .take(channels)
                .copied()
                .collect(),
        )
    }
}

/// Per-track mute/enable/ended flags. The renderer loads these on the audio
/// thread, so stopping or muting a track takes effect without republishing
/// the graph.
#[derive(Debug)]
pub struct Audibility {
    enabled: AtomicBool,
    muted: AtomicBool,
    closed: AtomicBool,
}

impl Audibility {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            muted: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Acquire)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Release);
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub fn audible(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self.enabled.load(Ordering::Acquire)
            && !self.muted.load(Ordering::Acquire)
    }

    fn fork(&self) -> Self {
        Self {
            enabled: AtomicBool::new(self.enabled()),
            muted: AtomicBool::new(self.muted()),
            closed: AtomicBool::new(false),
        }
    }
}

impl Default for Audibility {
    fn default() -> Self {
        Self::new()
    }
}

/// What the audio renderer needs from a live microphone track.
#[derive(Clone)]
pub struct RenderSource {
    pub queue: Arc<PcmQueue>,
    pub audibility: Arc<Audibility>,
    pub channels: u16,
    pub sample_rate: u32,
}

pub struct OpenedMic {
    queue: Arc<PcmQueue>,
    device: Arc<DeviceSlot>,
    pub sample_rate: u32,
    pub channels: u16,
    pub label: String,
}

struct DeviceSlot {
    device_key: String,
    queue: Arc<PcmQueue>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for DeviceSlot {
    fn drop(&mut self) {
        unregister_queue(&self.device_key, &self.queue);
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

#[derive(Clone)]
struct TrackBinding {
    audibility: Arc<Audibility>,
    source: RenderSource,
    /// Owns the capture. Dropping the last track stops the device thread.
    #[allow(dead_code)]
    device: Arc<DeviceSlot>,
    device_id: String,
    group_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackMic {
    pub device_id: String,
    pub group_id: String,
    pub sample_rate: u32,
    pub channels: u16,
}

thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Vec<MicrophoneInfo>>> =
        const { std::cell::RefCell::new(None) };
    static PENDING: std::cell::RefCell<HashMap<String, Vec<f32>>> =
        std::cell::RefCell::new(HashMap::new());
    static TRACKS: std::cell::RefCell<HashMap<String, TrackBinding>> =
        std::cell::RefCell::new(HashMap::new());
}

fn live_queues() -> &'static Mutex<HashMap<String, Vec<Arc<PcmQueue>>>> {
    static LIVE: std::sync::OnceLock<Mutex<HashMap<String, Vec<Arc<PcmQueue>>>>> =
        std::sync::OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_queue(device_key: &str, queue: &Arc<PcmQueue>) {
    let mut live = live_queues()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    live.entry(device_key.to_string())
        .or_default()
        .push(Arc::clone(queue));
}

fn unregister_queue(device_key: &str, queue: &Arc<PcmQueue>) {
    let mut live = live_queues()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(queues) = live.get_mut(device_key) {
        queues.retain(|existing| !Arc::ptr_eq(existing, queue));
        if queues.is_empty() {
            live.remove(device_key);
        }
    }
}

/// Input devices from the platform adapter, or the test override when one
/// is installed. `Some(vec![])` means the host has no microphones.
pub fn list_microphones() -> Vec<MicrophoneInfo> {
    if let Some(mics) = OVERRIDE.with(|slot| slot.borrow().clone()) {
        return mics;
    }
    native_list()
}

pub fn set_microphone_override(mics: Option<Vec<MicrophoneInfo>>) {
    OVERRIDE.with(|slot| *slot.borrow_mut() = mics);
}

/// Appends interleaved PCM for a microphone. Samples pushed before the
/// device is opened are delivered when it opens. Samples pushed while it is
/// open reach every live capture of that device.
pub fn push_microphone_samples(device_key: &str, samples: &[f32]) {
    let queues = live_queues()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(device_key)
        .cloned()
        .unwrap_or_default();
    if queues.is_empty() {
        PENDING.with(|pending| {
            pending
                .borrow_mut()
                .entry(device_key.to_string())
                .or_default()
                .extend_from_slice(samples);
        });
        return;
    }
    for queue in queues {
        queue.push_interleaved(samples);
    }
}

pub fn reset_overrides() {
    OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    PENDING.with(|pending| pending.borrow_mut().clear());
}

/// Drops every live track binding. The last binding for a device stops its
/// capture thread.
pub fn reset_tracks() {
    let bindings: Vec<TrackBinding> = TRACKS.with(|tracks| {
        tracks
            .borrow_mut()
            .drain()
            .map(|(_, binding)| binding)
            .collect()
    });
    for binding in bindings {
        binding.audibility.close();
    }
}

pub fn open_microphone(mic: &MicrophoneInfo, prefs: &CapturePrefs) -> Result<OpenedMic, MicError> {
    let overridden = OVERRIDE.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|mics| mics.iter().any(|item| item.device_key == mic.device_key))
    });
    if overridden {
        return open_override(mic, prefs);
    }
    native_open(mic, prefs)
}

fn open_override(mic: &MicrophoneInfo, prefs: &CapturePrefs) -> Result<OpenedMic, MicError> {
    let channels = mic.channels.max(1);
    let rate = mic.sample_rate.max(1);
    let windows = [RateWindow {
        channels,
        min_rate: rate,
        max_rate: rate,
    }];
    let (channels, rate) = select_format(&windows, channels, rate, prefs)?;
    let queue = PcmQueue::new(channels, rate);
    let pending = PENDING.with(|pending| pending.borrow_mut().remove(&mic.device_key));
    if let Some(samples) = pending {
        queue.push_interleaved(&samples);
    }
    register_queue(&mic.device_key, &queue);
    let device = Arc::new(DeviceSlot {
        device_key: mic.device_key.clone(),
        queue: Arc::clone(&queue),
        stop: Arc::new(AtomicBool::new(false)),
        thread: Mutex::new(None),
    });
    Ok(OpenedMic {
        queue,
        device,
        sample_rate: rate,
        channels,
        label: mic.label.clone(),
    })
}

pub fn bind_track(
    track_id: &str,
    opened: OpenedMic,
    device_id: String,
    group_id: String,
) -> Arc<Audibility> {
    let audibility = Arc::new(Audibility::new());
    let source = RenderSource {
        queue: Arc::clone(&opened.queue),
        audibility: Arc::clone(&audibility),
        channels: opened.channels,
        sample_rate: opened.sample_rate,
    };
    TRACKS.with(|tracks| {
        tracks.borrow_mut().insert(
            track_id.to_string(),
            TrackBinding {
                audibility: Arc::clone(&audibility),
                source,
                device: opened.device,
                device_id,
                group_id,
            },
        );
    });
    audibility
}

pub fn source_for_track(track_id: &str) -> Option<RenderSource> {
    TRACKS.with(|tracks| {
        tracks
            .borrow()
            .get(track_id)
            .map(|binding| binding.source.clone())
    })
}

pub fn track_mic(track_id: &str) -> Option<TrackMic> {
    TRACKS.with(|tracks| {
        tracks.borrow().get(track_id).map(|binding| TrackMic {
            device_id: binding.device_id.clone(),
            group_id: binding.group_id.clone(),
            sample_rate: binding.source.sample_rate,
            channels: binding.source.channels,
        })
    })
}

pub fn audibility_of(track_id: &str) -> Option<Arc<Audibility>> {
    TRACKS.with(|tracks| {
        tracks
            .borrow()
            .get(track_id)
            .map(|binding| Arc::clone(&binding.audibility))
    })
}

/// Shares the capture with a cloned track. The clone has its own
/// enabled/muted/ended flags and hears the same PCM.
pub fn clone_track(from: &str, to: &str) -> Option<Arc<Audibility>> {
    TRACKS.with(|tracks| {
        let existing = tracks.borrow().get(from).cloned()?;
        let audibility = Arc::new(existing.audibility.fork());
        let mut binding = existing;
        binding.source.audibility = Arc::clone(&audibility);
        binding.audibility = Arc::clone(&audibility);
        tracks.borrow_mut().insert(to.to_string(), binding);
        Some(audibility)
    })
}

pub fn release_track(track_id: &str) {
    if let Some(binding) = TRACKS.with(|tracks| tracks.borrow_mut().remove(track_id)) {
        binding.audibility.close();
    }
}

pub fn live_track_count() -> usize {
    TRACKS.with(|tracks| tracks.borrow().len())
}

fn native_list() -> Vec<MicrophoneInfo> {
    #[cfg(all(
        feature = "web-media-advanced",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    {
        if !cfg!(test) || std::env::var_os("W3COS_AUDIO_DEVICE_TESTS").is_some() {
            return native::list();
        }
    }
    Vec::new()
}

fn native_open(mic: &MicrophoneInfo, prefs: &CapturePrefs) -> Result<OpenedMic, MicError> {
    #[cfg(all(
        feature = "web-media-advanced",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    {
        return native::open(mic, prefs);
    }
    #[cfg(not(all(
        feature = "web-media-advanced",
        any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    )))]
    {
        let _ = (mic, prefs);
        Err(MicError::not_found(
            "this build has no native microphone adapter",
        ))
    }
}

#[cfg(all(
    feature = "web-media-advanced",
    any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    )
))]
mod native {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    use super::{
        CapturePrefs, DeviceSlot, MicError, MicrophoneInfo, OpenedMic, PcmQueue, RateWindow,
        register_queue, select_format,
    };

    struct NativeMode {
        window: RateWindow,
        format: cpal::SampleFormat,
    }

    struct Ready {
        queue: Arc<PcmQueue>,
        stop: Arc<AtomicBool>,
        sample_rate: u32,
        channels: u16,
    }

    pub(super) fn list() -> Vec<MicrophoneInfo> {
        let host = cpal::default_host();
        let host_name = host.id().name();
        let default_name = host
            .default_input_device()
            .and_then(|device| device.name().ok());
        let Ok(devices) = host.input_devices() else {
            return Vec::new();
        };
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut mics = Vec::new();
        for device in devices {
            let name = device.name().unwrap_or_else(|_| "Microphone".into());
            let ordinal = *seen.get(&name).unwrap_or(&0);
            seen.insert(name.clone(), ordinal + 1);
            let Ok(config) = device.default_input_config() else {
                continue;
            };
            let (device_key, group_key) = identity(host_name, &name, ordinal);
            mics.push(MicrophoneInfo {
                is_default: ordinal == 0 && default_name.as_deref() == Some(name.as_str()),
                label: name,
                device_key,
                group_key,
                sample_rate: config.sample_rate().0,
                channels: config.channels(),
            });
        }
        if !mics.iter().any(|mic| mic.is_default) {
            if let Some(first) = mics.first_mut() {
                first.is_default = true;
            }
        }
        mics
    }

    pub(super) fn open(mic: &MicrophoneInfo, prefs: &CapturePrefs) -> Result<OpenedMic, MicError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<Ready, MicError>>();
        let device_key = mic.device_key.clone();
        let prefs = prefs.clone();
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("w3cos-audio-input".into())
            .spawn(move || {
                let stream = match build_stream(&device_key, &prefs, &thread_stop) {
                    Ok((stream, ready)) => {
                        let _ = ready_tx.send(Ok(ready));
                        stream
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                while !thread_stop.load(Ordering::Acquire) {
                    thread::park_timeout(Duration::from_millis(100));
                }
                drop(stream);
            })
            .map_err(|error| {
                MicError::not_readable(format!("cannot spawn the microphone thread: {error}"))
            })?;
        let ready = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| MicError::not_readable("the microphone did not start in time"))
            .and_then(|result| result);
        match ready {
            Ok(ready) => {
                register_queue(&mic.device_key, &ready.queue);
                let pending =
                    super::PENDING.with(|pending| pending.borrow_mut().remove(&mic.device_key));
                if let Some(samples) = pending {
                    ready.queue.push_interleaved(&samples);
                }
                let device = Arc::new(DeviceSlot {
                    device_key: mic.device_key.clone(),
                    queue: Arc::clone(&ready.queue),
                    stop: ready.stop,
                    thread: Mutex::new(Some(thread)),
                });
                Ok(OpenedMic {
                    queue: ready.queue,
                    device,
                    sample_rate: ready.sample_rate,
                    channels: ready.channels,
                    label: mic.label.clone(),
                })
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                thread.thread().unpark();
                let _ = thread.join();
                Err(error)
            }
        }
    }

    fn identity(host_name: &str, device_name: &str, ordinal: usize) -> (String, String) {
        let suffix = if ordinal == 0 {
            String::new()
        } else {
            format!("#{ordinal}")
        };
        (
            format!("cpal:{host_name}:{device_name}{suffix}"),
            format!("cpal-group:{host_name}:{device_name}{suffix}"),
        )
    }

    fn find_device(device_key: &str) -> Result<cpal::Device, MicError> {
        let host = cpal::default_host();
        let host_name = host.id().name();
        let devices = host.input_devices().map_err(|error| {
            MicError::not_readable(format!("cannot enumerate audio input devices: {error}"))
        })?;
        let mut seen: HashMap<String, usize> = HashMap::new();
        for device in devices {
            let name = device.name().unwrap_or_else(|_| "Microphone".into());
            let ordinal = *seen.get(&name).unwrap_or(&0);
            seen.insert(name.clone(), ordinal + 1);
            if identity(host_name, &name, ordinal).0 == device_key {
                return Ok(device);
            }
        }
        Err(MicError::not_found(format!(
            "audio input device {device_key} is no longer available"
        )))
    }

    fn modes_of(
        device: &cpal::Device,
    ) -> Result<(cpal::SupportedStreamConfig, Vec<NativeMode>), MicError> {
        let default = device.default_input_config().map_err(|error| {
            MicError::not_readable(format!(
                "cannot query the microphone configuration: {error}"
            ))
        })?;
        let supported = device.supported_input_configs().map_err(|error| {
            MicError::not_readable(format!("cannot query microphone sample rates: {error}"))
        })?;
        let modes = supported
            .map(|range| NativeMode {
                window: RateWindow {
                    channels: range.channels(),
                    min_rate: range.min_sample_rate().0,
                    max_rate: range.max_sample_rate().0,
                },
                format: range.sample_format(),
            })
            .collect::<Vec<_>>();
        Ok((default, modes))
    }

    fn choose_format(
        modes: &[NativeMode],
        channels: u16,
        rate: u32,
    ) -> Result<cpal::SampleFormat, MicError> {
        let mut matching: Vec<cpal::SampleFormat> = modes
            .iter()
            .filter(|mode| {
                mode.window.channels == channels
                    && rate >= mode.window.min_rate
                    && rate <= mode.window.max_rate
            })
            .map(|mode| mode.format)
            .collect();
        matching.sort_by_key(|format| format_rank(*format));
        matching.into_iter().next().ok_or_else(|| {
            MicError::not_readable("the microphone cannot open the negotiated format")
        })
    }

    fn format_rank(format: cpal::SampleFormat) -> u8 {
        match format {
            cpal::SampleFormat::F32 => 0,
            cpal::SampleFormat::I16 => 1,
            cpal::SampleFormat::I32 => 2,
            cpal::SampleFormat::U16 => 3,
            cpal::SampleFormat::F64 => 4,
            _ => 5,
        }
    }

    fn build_stream(
        device_key: &str,
        prefs: &CapturePrefs,
        stop: &Arc<AtomicBool>,
    ) -> Result<(cpal::Stream, Ready), MicError> {
        let device = find_device(device_key)?;
        let (default, modes) = modes_of(&device)?;
        let windows: Vec<RateWindow> = modes.iter().map(|mode| mode.window).collect();
        let (channels, rate) =
            select_format(&windows, default.channels(), default.sample_rate().0, prefs)?;
        let format = choose_format(&modes, channels, rate)?;
        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(rate),
            buffer_size: cpal::BufferSize::Default,
        };
        let queue = PcmQueue::new(channels, rate);
        let callback_queue = Arc::clone(&queue);
        let on_error = |error: cpal::StreamError| {
            eprintln!("[w3cos] warning: microphone stream error: {error}");
        };
        let stream = match format {
            cpal::SampleFormat::F32 => {
                input_stream::<f32, _>(&device, &config, callback_queue, on_error, |sample| sample)
            }
            cpal::SampleFormat::I16 => {
                input_stream::<i16, _>(&device, &config, callback_queue, on_error, |sample| {
                    f32::from(sample) / 32768.0
                })
            }
            cpal::SampleFormat::U16 => {
                input_stream::<u16, _>(&device, &config, callback_queue, on_error, |sample| {
                    (f32::from(sample) - 32768.0) / 32768.0
                })
            }
            cpal::SampleFormat::I32 => {
                input_stream::<i32, _>(&device, &config, callback_queue, on_error, |sample| {
                    sample as f32 / 2147483648.0
                })
            }
            cpal::SampleFormat::F64 => {
                input_stream::<f64, _>(&device, &config, callback_queue, on_error, |sample| {
                    sample as f32
                })
            }
            other => {
                return Err(MicError::not_readable(format!(
                    "unsupported microphone sample format {other:?}"
                )));
            }
        }?;
        stream.play().map_err(|error| {
            MicError::not_readable(format!("cannot start the microphone stream: {error}"))
        })?;
        Ok((
            stream,
            Ready {
                queue,
                stop: Arc::clone(stop),
                sample_rate: rate,
                channels,
            },
        ))
    }

    fn input_stream<T, F>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        queue: Arc<PcmQueue>,
        on_error: impl FnMut(cpal::StreamError) + Send + 'static,
        convert: F,
    ) -> Result<cpal::Stream, MicError>
    where
        T: cpal::SizedSample + Send + 'static,
        F: Fn(T) -> f32 + Send + Copy + 'static,
    {
        device
            .build_input_stream(
                config,
                move |data: &[T], _: &cpal::InputCallbackInfo| {
                    let converted: Vec<f32> = data.iter().copied().map(convert).collect();
                    queue.push_interleaved(&converted);
                },
                on_error,
                None,
            )
            .map_err(|error| {
                MicError::not_readable(format!("cannot open the microphone stream: {error}"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono_8k() -> MicrophoneInfo {
        MicrophoneInfo {
            label: "Fixture Mic".into(),
            device_key: "fixture:mic".into(),
            group_key: "fixture:group".into(),
            sample_rate: 8_000,
            channels: 1,
            is_default: true,
        }
    }

    fn windows_of(mic: &MicrophoneInfo) -> Vec<RateWindow> {
        vec![RateWindow {
            channels: mic.channels,
            min_rate: mic.sample_rate,
            max_rate: mic.sample_rate,
        }]
    }

    #[test]
    fn format_selection_honours_exact_and_falls_back_for_ideal() {
        let mic = mono_8k();
        let windows = windows_of(&mic);
        let (channels, rate) = select_format(&windows, 1, 8_000, &CapturePrefs::default()).unwrap();
        assert_eq!((channels, rate), (1, 8_000));

        let mut ideal = CapturePrefs::default();
        ideal.sample_rate = Some(16_000);
        let (_, rate) = select_format(&windows, 1, 8_000, &ideal).unwrap();
        assert_eq!(rate, 8_000);

        let mut exact = CapturePrefs::default();
        exact.sample_rate = Some(16_000);
        exact.sample_rate_exact = true;
        let error = select_format(&windows, 1, 8_000, &exact).unwrap_err();
        assert_eq!(error.name, "OverconstrainedError");
        assert_eq!(error.constraint, Some("sampleRate"));

        let mut channels_exact = CapturePrefs::default();
        channels_exact.channels = Some(2);
        channels_exact.channels_exact = true;
        let error = select_format(&windows, 1, 8_000, &channels_exact).unwrap_err();
        assert_eq!(error.constraint, Some("channelCount"));

        let mut too_fast = CapturePrefs::default();
        too_fast.sample_rate_min = Some(16_000);
        let error = select_format(&windows, 1, 8_000, &too_fast).unwrap_err();
        assert_eq!(error.constraint, Some("sampleRate"));
    }

    #[test]
    fn queue_broadcasts_to_independent_cursors_and_drops_old_frames() {
        let queue = PcmQueue::new(1, 8);
        queue.push_interleaved(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(queue.start_cursor(), 0);
        let mut first = queue.start_cursor();
        let mut second = queue.start_cursor();
        assert_eq!(queue.read_frames(&mut first, 2), vec![1.0, 2.0]);
        assert_eq!(queue.read_frames(&mut second, 4), vec![1.0, 2.0, 3.0, 4.0]);
        assert!(queue.read_frames(&mut second, 1).is_empty());
        queue.push_interleaved(&[5.0]);
        assert_eq!(queue.read_frames(&mut first, 4), vec![3.0, 4.0, 5.0]);

        let capped = PcmQueue::new(1, 4);
        capped.push_interleaved(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(capped.buffered_frames(), 4);
        let mut cursor = 0;
        assert_eq!(capped.read_frames(&mut cursor, 4), vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn override_microphone_delivers_samples_pushed_before_open() {
        reset_overrides();
        reset_tracks();
        let mic = mono_8k();
        set_microphone_override(Some(vec![mic.clone()]));
        push_microphone_samples(&mic.device_key, &[0.25, -0.5]);
        let opened = open_microphone(&mic, &CapturePrefs::default()).unwrap();
        assert_eq!(opened.sample_rate, 8_000);
        assert_eq!(opened.channels, 1);
        let mut cursor = opened.queue.start_cursor();
        assert_eq!(opened.queue.read_frames(&mut cursor, 2), vec![0.25, -0.5]);
        push_microphone_samples(&mic.device_key, &[0.125]);
        assert_eq!(opened.queue.read_frames(&mut cursor, 1), vec![0.125]);
        reset_tracks();
        reset_overrides();
    }

    #[test]
    fn default_input_device_captures_when_enabled() {
        if std::env::var_os("W3COS_AUDIO_DEVICE_TESTS").is_none() {
            eprintln!("skipping microphone test: set W3COS_AUDIO_DEVICE_TESTS=1");
            return;
        }
        reset_overrides();
        let mics = list_microphones();
        let Some(mic) = mics
            .iter()
            .find(|mic| mic.is_default)
            .or_else(|| mics.first())
        else {
            eprintln!("skipping microphone test: no input device");
            return;
        };
        let opened = match open_microphone(mic, &CapturePrefs::default()) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("skipping microphone test: {}", error.message);
                return;
            }
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(opened.sample_rate > 0);
        assert!(opened.channels > 0);
        assert!(
            opened.queue.buffered_frames() > 0,
            "device {} captured no frames",
            opened.label
        );
    }
}
