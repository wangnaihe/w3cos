//! Geolocation acquisition thread.
//!
//! `getCurrentPosition` / `watchPosition` decide permission, cache, and
//! `timeout: 0` on the JavaScript thread. Anything that has to wait for a
//! device runs here and comes back through [`take_events`], which the DOM
//! event loop drains. The thread never invents a latitude or longitude.
//!
//! Unit tests install a mock source. The real platform session is used only
//! outside tests, or when `W3COS_GEOLOCATION_DEVICE_TESTS=1`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use std::cell::RefCell;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fix {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy: f64,
    pub altitude: Option<f64>,
    pub altitude_accuracy: Option<f64>,
    pub heading: Option<f64>,
    pub speed: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FixError {
    PermissionDenied,
    Unavailable,
    Timeout,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PollFix {
    Pending,
    Ready(Result<Fix, FixError>),
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderEvent {
    pub id: u64,
    pub watch: bool,
    pub high_accuracy: bool,
    pub result: Result<Fix, FixError>,
}

#[derive(Clone, Debug)]
pub(crate) struct MockStep {
    pub delay: Duration,
    pub result: Result<Fix, FixError>,
}

struct Scripted {
    ready_at: Instant,
    result: Result<Fix, FixError>,
}

struct Job {
    id: u64,
    watch: bool,
    high_accuracy: bool,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
    cancel: Arc<AtomicBool>,
    scripted: Option<Scripted>,
    platform: bool,
    finished: bool,
}

enum Cmd {
    Start(Job),
    Shutdown,
}

struct Shared {
    events: Mutex<Vec<ProviderEvent>>,
    next_deadline: Mutex<Option<Instant>>,
    mock_enabled: AtomicBool,
    steps: Mutex<VecDeque<MockStep>>,
    calls: AtomicUsize,
    accuracies: Mutex<Vec<bool>>,
    cancels: Mutex<Vec<(u64, Arc<AtomicBool>)>>,
    stop: AtomicBool,
    running: AtomicBool,
}

struct Local {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    rx: Option<Receiver<Cmd>>,
    join: Option<JoinHandle<()>>,
}

thread_local! {
    static LOCAL: RefCell<Option<Local>> = const { RefCell::new(None) };
}

impl Drop for Local {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Shared {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            next_deadline: Mutex::new(None),
            mock_enabled: AtomicBool::new(false),
            steps: Mutex::new(VecDeque::new()),
            calls: AtomicUsize::new(0),
            accuracies: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
        }
    }
}

pub(crate) fn platform_enabled() -> bool {
    if !desktop_adapter() {
        return false;
    }
    #[cfg(test)]
    {
        std::env::var_os("W3COS_GEOLOCATION_DEVICE_TESTS").is_some_and(|value| value == "1")
    }
    #[cfg(not(test))]
    {
        true
    }
}

fn desktop_adapter() -> bool {
    #[cfg(all(
        feature = "web-geolocation",
        any(
            all(target_os = "linux", not(target_env = "ohos")),
            target_os = "macos",
            target_os = "windows"
        )
    ))]
    {
        true
    }
    #[cfg(not(all(
        feature = "web-geolocation",
        any(
            all(target_os = "linux", not(target_env = "ohos")),
            target_os = "macos",
            target_os = "windows"
        )
    )))]
    {
        false
    }
}

pub(crate) fn available() -> bool {
    mock_enabled() || platform_enabled()
}

fn mock_enabled() -> bool {
    LOCAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|local| local.shared.mock_enabled.load(Ordering::SeqCst))
    })
}

#[allow(dead_code)]
pub(crate) fn enable_mock() {
    with_local(|local| {
        local.shared.mock_enabled.store(true, Ordering::SeqCst);
    });
}

pub(crate) fn push_mock(delay: Duration, result: Result<Fix, FixError>) {
    with_local(|local| {
        local.shared.mock_enabled.store(true, Ordering::SeqCst);
        if let Ok(mut steps) = local.shared.steps.lock() {
            steps.push_back(MockStep { delay, result });
        }
    });
}

pub(crate) fn mock_calls() -> usize {
    LOCAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|local| local.shared.calls.load(Ordering::SeqCst))
            .unwrap_or(0)
    })
}

pub(crate) fn mock_accuracies() -> Vec<bool> {
    LOCAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|local| {
                local
                    .shared
                    .accuracies
                    .lock()
                    .ok()
                    .map(|values| values.clone())
            })
            .unwrap_or_default()
    })
}

pub(crate) fn submit(id: u64, watch: bool, high_accuracy: bool, timeout: Option<Duration>) {
    with_local(|local| {
        let shared = Arc::clone(&local.shared);
        shared.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut seen) = shared.accuracies.lock() {
            seen.push(high_accuracy);
        }
        let scripted = pop_scripted(&shared);
        let platform =
            scripted.is_none() && !shared.mock_enabled.load(Ordering::SeqCst) && platform_enabled();
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut cancels) = shared.cancels.lock() {
            cancels.push((id, Arc::clone(&cancel)));
        }
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let job = Job {
            id,
            watch,
            high_accuracy,
            timeout,
            deadline,
            cancel,
            scripted,
            platform,
            finished: false,
        };
        ensure_running(local);
        let _ = local.tx.send(Cmd::Start(job));
    });
}

pub(crate) fn cancel(id: u64) {
    let Some(shared) = LOCAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|local| Arc::clone(&local.shared))
    }) else {
        return;
    };
    if let Ok(cancels) = shared.cancels.lock() {
        for (job_id, flag) in cancels.iter() {
            if *job_id == id {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }
}

pub(crate) fn take_events() -> Vec<ProviderEvent> {
    let Some(shared) = LOCAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|local| Arc::clone(&local.shared))
    }) else {
        return Vec::new();
    };
    shared
        .events
        .lock()
        .map(|mut events| std::mem::take(&mut *events))
        .unwrap_or_default()
}

pub(crate) fn next_deadline() -> Option<Instant> {
    let shared = LOCAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|local| Arc::clone(&local.shared))
    })?;
    shared
        .next_deadline
        .lock()
        .ok()
        .and_then(|deadline| *deadline)
}

pub(crate) fn reset() {
    let local = LOCAL.with(|slot| slot.borrow_mut().take());
    drop(local);
}

fn pop_scripted(shared: &Shared) -> Option<Scripted> {
    let step = shared.steps.lock().ok()?.pop_front()?;
    Some(Scripted {
        ready_at: Instant::now() + step.delay,
        result: step.result,
    })
}

fn with_local<T>(f: impl FnOnce(&mut Local) -> T) -> T {
    LOCAL.with(|slot| {
        {
            let mut local = slot.borrow_mut();
            if local.is_none() {
                *local = Some(Local::create());
            }
        }
        let mut local = slot.borrow_mut();
        f(local.as_mut().expect("geolocation provider"))
    })
}

impl Local {
    fn create() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            shared: Arc::new(Shared::new()),
            tx,
            rx: Some(rx),
            join: None,
        }
    }
}

fn ensure_running(local: &mut Local) {
    if local.shared.running.load(Ordering::SeqCst) {
        return;
    }
    let Some(rx) = local.rx.take() else {
        return;
    };
    local.shared.stop.store(false, Ordering::SeqCst);
    local.shared.running.store(true, Ordering::SeqCst);
    let shared = Arc::clone(&local.shared);
    match std::thread::Builder::new()
        .name("w3cos-geolocation".into())
        .spawn(move || worker(shared, rx))
    {
        Ok(handle) => local.join = Some(handle),
        Err(err) => {
            local.shared.running.store(false, Ordering::SeqCst);
            eprintln!("[w3cos] warning: geolocation provider failed to start: {err}");
        }
    }
}

fn worker(shared: Arc<Shared>, rx: Receiver<Cmd>) {
    let mut jobs: Vec<Job> = Vec::new();
    let mut session: Option<Platform> = None;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let mut shutdown = false;
        loop {
            match rx.try_recv() {
                Ok(Cmd::Shutdown) => shutdown = true,
                Ok(Cmd::Start(job)) => jobs.push(job),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => shutdown = true,
            }
            if shutdown {
                break;
            }
        }
        if shutdown || shared.stop.load(Ordering::SeqCst) {
            break;
        }
        advance(&shared, &mut jobs, &mut session);
        jobs.retain(|job| !job.finished && !job.cancel.load(Ordering::SeqCst));
        publish_deadline(&shared, &jobs);
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let wait = wait_for(&jobs);
        match rx.recv_timeout(wait) {
            Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Cmd::Start(job)) => jobs.push(job),
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    if let Some(session) = session.as_mut() {
        shutdown_platform(session);
    }
    shared.running.store(false, Ordering::SeqCst);
}

fn wait_for(jobs: &[Job]) -> Duration {
    if jobs.is_empty() {
        return Duration::from_secs(3600);
    }
    let now = Instant::now();
    let mut wait = Duration::from_millis(200);
    for job in jobs {
        if let Some(scripted) = &job.scripted {
            wait = wait.min(scripted.ready_at.saturating_duration_since(now));
        }
        if let Some(deadline) = job.deadline {
            wait = wait.min(deadline.saturating_duration_since(now));
        }
    }
    if wait.is_zero() {
        Duration::from_millis(1)
    } else {
        wait
    }
}

fn publish_deadline(shared: &Shared, jobs: &[Job]) {
    // Wake the DOM loop while a request is in flight, including watches whose
    // timeout is infinite. A fix still arrives through `poke_event_loop`.
    let heartbeat = Instant::now() + Duration::from_millis(50);
    let next = if jobs.is_empty() {
        None
    } else {
        Some(
            jobs.iter()
                .filter_map(|job| job.deadline)
                .min()
                .map(|deadline| deadline.min(heartbeat))
                .unwrap_or(heartbeat),
        )
    };
    if let Ok(mut slot) = shared.next_deadline.lock() {
        *slot = next;
    }
}

fn advance(shared: &Shared, jobs: &mut [Job], session: &mut Option<Platform>) {
    let mut events = Vec::new();
    let wants_platform = jobs.iter().any(|job| {
        job.platform
            && job.scripted.is_none()
            && !job.finished
            && !job.cancel.load(Ordering::SeqCst)
    });
    let platform_fix = if wants_platform {
        let high = jobs.iter().any(|job| {
            job.platform && job.high_accuracy && !job.finished && !job.cancel.load(Ordering::SeqCst)
        });
        let live = session.get_or_insert_with(open_platform);
        Some(poll_platform(live, high))
    } else {
        None
    };

    for job in jobs.iter_mut() {
        if job.finished || job.cancel.load(Ordering::SeqCst) {
            continue;
        }
        if let Some(scripted) = &job.scripted {
            let due = Instant::now() >= scripted.ready_at
                && job
                    .deadline
                    .map(|deadline| scripted.ready_at <= deadline)
                    .unwrap_or(true);
            if due {
                let result = sanitize_result(scripted.result.clone());
                job.scripted = None;
                events.push(job.event(result.clone()));
                finish_after(job, &result, shared, true);
                continue;
            }
        }
        if job.platform && job.scripted.is_none() {
            if let Some(PollFix::Ready(result)) = &platform_fix {
                let result = sanitize_result(result.clone());
                events.push(job.event(result.clone()));
                finish_after(job, &result, shared, false);
                continue;
            }
        }
        if let Some(deadline) = job.deadline
            && Instant::now() >= deadline
            && job
                .scripted
                .as_ref()
                .map(|scripted| scripted.ready_at > deadline)
                .unwrap_or(true)
        {
            events.push(job.event(Err(FixError::Timeout)));
            if job.watch {
                job.rearm();
            } else {
                job.finished = true;
            }
            continue;
        }
        if job.scripted.is_none() && !job.platform {
            events.push(job.event(Err(FixError::Unavailable)));
            job.finished = true;
        }
    }

    if !events.is_empty() {
        if let Ok(mut queued) = shared.events.lock() {
            queued.extend(events);
        }
        crate::uitest::poke_event_loop();
    }
}

fn finish_after(job: &mut Job, result: &Result<Fix, FixError>, shared: &Shared, from_mock: bool) {
    match result {
        Ok(_) if job.watch => {
            if from_mock {
                job.scripted = pop_scripted(shared);
                if job.scripted.is_some() {
                    shared.calls.fetch_add(1, Ordering::SeqCst);
                    if let Ok(mut seen) = shared.accuracies.lock() {
                        seen.push(job.high_accuracy);
                    }
                }
            }
            job.rearm();
        }
        Err(FixError::Timeout) if job.watch => job.rearm(),
        _ => job.finished = true,
    }
}

impl Job {
    fn event(&self, result: Result<Fix, FixError>) -> ProviderEvent {
        ProviderEvent {
            id: self.id,
            watch: self.watch,
            high_accuracy: self.high_accuracy,
            result,
        }
    }

    fn rearm(&mut self) {
        self.deadline = self.timeout.map(|timeout| Instant::now() + timeout);
    }
}

fn sanitize_result(result: Result<Fix, FixError>) -> Result<Fix, FixError> {
    match result {
        Ok(fix) if fix_is_real(&fix) => Ok(normalize_fix(fix)),
        Ok(_) => Err(FixError::Unavailable),
        Err(error) => Err(error),
    }
}

pub(crate) fn fix_is_real(fix: &Fix) -> bool {
    (-90.0..=90.0).contains(&fix.latitude)
        && (-180.0..=180.0).contains(&fix.longitude)
        && fix.latitude.is_finite()
        && fix.longitude.is_finite()
        && fix.accuracy.is_finite()
        && fix.accuracy >= 0.0
}

fn normalize_fix(mut fix: Fix) -> Fix {
    fix.altitude = fix.altitude.filter(|value| value.is_finite());
    fix.altitude_accuracy = fix
        .altitude_accuracy
        .filter(|value| value.is_finite() && *value >= 0.0 && fix.altitude.is_some());
    fix.heading = fix
        .heading
        .filter(|value| value.is_finite() && (0.0..360.0).contains(value));
    fix.speed = fix.speed.filter(|value| value.is_finite() && *value >= 0.0);
    fix
}

#[cfg(all(
    feature = "web-geolocation",
    target_os = "linux",
    not(target_env = "ohos")
))]
type Platform = crate::geolocation_linux::Session;
#[cfg(all(feature = "web-geolocation", target_os = "macos"))]
type Platform = crate::geolocation_macos::Session;
#[cfg(all(feature = "web-geolocation", target_os = "windows"))]
type Platform = crate::geolocation_windows::Session;
#[cfg(not(any(
    all(
        feature = "web-geolocation",
        target_os = "linux",
        not(target_env = "ohos")
    ),
    all(feature = "web-geolocation", target_os = "macos"),
    all(feature = "web-geolocation", target_os = "windows")
)))]
struct Platform;

#[cfg(any(
    all(
        feature = "web-geolocation",
        target_os = "linux",
        not(target_env = "ohos")
    ),
    all(feature = "web-geolocation", target_os = "macos"),
    all(feature = "web-geolocation", target_os = "windows")
))]
fn open_platform() -> Platform {
    Platform::open()
}

#[cfg(not(any(
    all(
        feature = "web-geolocation",
        target_os = "linux",
        not(target_env = "ohos")
    ),
    all(feature = "web-geolocation", target_os = "macos"),
    all(feature = "web-geolocation", target_os = "windows")
)))]
fn open_platform() -> Platform {
    Platform
}

#[cfg(any(
    all(
        feature = "web-geolocation",
        target_os = "linux",
        not(target_env = "ohos")
    ),
    all(feature = "web-geolocation", target_os = "macos"),
    all(feature = "web-geolocation", target_os = "windows")
))]
fn poll_platform(session: &mut Platform, high_accuracy: bool) -> PollFix {
    session.poll(high_accuracy)
}

#[cfg(not(any(
    all(
        feature = "web-geolocation",
        target_os = "linux",
        not(target_env = "ohos")
    ),
    all(feature = "web-geolocation", target_os = "macos"),
    all(feature = "web-geolocation", target_os = "windows")
)))]
fn poll_platform(session: &mut Platform, high_accuracy: bool) -> PollFix {
    let _ = (session, high_accuracy);
    PollFix::Ready(Err(FixError::Unavailable))
}

#[cfg(any(
    all(
        feature = "web-geolocation",
        target_os = "linux",
        not(target_env = "ohos")
    ),
    all(feature = "web-geolocation", target_os = "macos"),
    all(feature = "web-geolocation", target_os = "windows")
))]
fn shutdown_platform(session: &mut Platform) {
    session.shutdown();
}

#[cfg(not(any(
    all(
        feature = "web-geolocation",
        target_os = "linux",
        not(target_env = "ohos")
    ),
    all(feature = "web-geolocation", target_os = "macos"),
    all(feature = "web-geolocation", target_os = "windows")
)))]
fn shutdown_platform(session: &mut Platform) {
    let _ = session;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_finite_in_range_coordinates_count_as_a_fix() {
        let fix = Fix {
            latitude: 31.23,
            longitude: 121.47,
            accuracy: 12.0,
            altitude: Some(f64::NAN),
            altitude_accuracy: Some(-1.0),
            heading: Some(400.0),
            speed: Some(-5.0),
        };
        let normalized = normalize_fix(fix);
        assert!(normalized.altitude.is_none());
        assert!(normalized.altitude_accuracy.is_none());
        assert!(normalized.heading.is_none());
        assert!(normalized.speed.is_none());
        assert!(!fix_is_real(&Fix {
            latitude: 120.0,
            longitude: 0.0,
            accuracy: 1.0,
            altitude: None,
            altitude_accuracy: None,
            heading: None,
            speed: None,
        }));
    }
}
