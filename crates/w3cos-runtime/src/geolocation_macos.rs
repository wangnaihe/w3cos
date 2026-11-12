//! macOS Core Location adapter for `navigator.geolocation`.
//!
//! `CLLocationManager` runs on the provider thread, which drives a
//! `CFRunLoop` so delegate callbacks are delivered there. Authorization is
//! when-in-use only (`requestWhenInUseAuthorization`). A denied or restricted
//! status is `PERMISSION_DENIED`. Location services switched off, or a manager
//! that cannot be created, is `POSITION_UNAVAILABLE`.
//!
//! The app bundle must describe why it uses location, or Core Location refuses
//! the prompt:
//!
//! * `NSLocationWhenInUseUsageDescription`
//! * `NSLocationUsageDescription` (older macOS still reads this key)
//!
//! Sandboxed apps also need the entitlement
//! `com.apple.security.personal-information.location`. This module does not
//! embed an Info.plist; the host bundle has to.
//!
//! iOS is intentionally not wired here.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_location::{
    CLAuthorizationStatus, CLLocation, CLLocationManager, CLLocationManagerDelegate,
    kCLDistanceFilterNone, kCLLocationAccuracyBest, kCLLocationAccuracyThreeKilometers,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};

use crate::geolocation_provider::{Fix, FixError, PollFix};

const CL_ERROR_LOCATION_UNKNOWN: isize = 0;
const CL_ERROR_DENIED: isize = 1;
const AUTH_WAIT: Duration = Duration::from_secs(60);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
}

struct Slot {
    fixes: Mutex<Vec<Fix>>,
    failure: Mutex<Option<FixError>>,
    auth_changed: AtomicBool,
}

struct DelegateIvars {
    slot: Arc<Slot>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = DelegateIvars]
    struct W3cosLocationDelegate;

    unsafe impl NSObjectProtocol for W3cosLocationDelegate {}

    unsafe impl CLLocationManagerDelegate for W3cosLocationDelegate {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update_locations(
            &self,
            _manager: &CLLocationManager,
            locations: &NSArray<CLLocation>,
        ) {
            let count = unsafe { locations.count() };
            if count == 0 {
                return;
            }
            let location = unsafe { locations.objectAtIndex(count - 1) };
            if let Some(fix) = fix_from_location(&location) {
                if let Ok(mut fixes) = self.ivars().slot.fixes.lock() {
                    fixes.push(fix);
                }
            }
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        fn did_fail(&self, _manager: &CLLocationManager, error: &NSError) {
            let code = unsafe { error.code() };
            let mapped = if code == CL_ERROR_DENIED {
                Some(FixError::PermissionDenied)
            } else if code == CL_ERROR_LOCATION_UNKNOWN {
                None
            } else {
                Some(FixError::Unavailable)
            };
            if let Some(mapped) = mapped
                && let Ok(mut failure) = self.ivars().slot.failure.lock()
            {
                *failure = Some(mapped);
            }
        }

        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn did_change_authorization(&self, _manager: &CLLocationManager) {
            self.ivars().slot.auth_changed.store(true, Ordering::SeqCst);
        }
    }
);

impl W3cosLocationDelegate {
    fn new(slot: Arc<Slot>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { slot });
        unsafe { msg_send![super(this), init] }
    }
}

pub(crate) struct Session {
    manager: Option<Retained<CLLocationManager>>,
    delegate: Option<Retained<W3cosLocationDelegate>>,
    slot: Arc<Slot>,
    phase: Phase,
    requested_auth: bool,
    auth_started: Option<Instant>,
    high_accuracy: Option<bool>,
    updates: bool,
}

enum Phase {
    Authorize,
    Updating,
    Failed(FixError),
}

impl Session {
    pub(crate) fn open() -> Self {
        Self {
            manager: None,
            delegate: None,
            slot: Arc::new(Slot {
                fixes: Mutex::new(Vec::new()),
                failure: Mutex::new(None),
                auth_changed: AtomicBool::new(false),
            }),
            phase: Phase::Authorize,
            requested_auth: false,
            auth_started: None,
            high_accuracy: None,
            updates: false,
        }
    }

    pub(crate) fn poll(&mut self, high_accuracy: bool) -> PollFix {
        if let Phase::Failed(error) = self.phase {
            return PollFix::Ready(Err(error));
        }
        if self.manager.is_none() {
            let manager = unsafe { CLLocationManager::new() };
            let delegate = W3cosLocationDelegate::new(Arc::clone(&self.slot));
            let protocol = ProtocolObject::from_ref(&*delegate);
            unsafe { manager.setDelegate(Some(protocol)) };
            self.delegate = Some(delegate);
            self.manager = Some(manager);
            self.auth_started = Some(Instant::now());
        }
        self.apply_accuracy(high_accuracy);
        if !unsafe { CLLocationManager::locationServicesEnabled_class() } {
            self.phase = Phase::Failed(FixError::Unavailable);
            return PollFix::Ready(Err(FixError::Unavailable));
        }
        let Some(manager) = self.manager.clone() else {
            return PollFix::Ready(Err(FixError::Unavailable));
        };
        pump(0.05);
        if let Ok(mut failure) = self.slot.failure.lock()
            && let Some(error) = failure.take()
        {
            if matches!(error, FixError::PermissionDenied) {
                self.phase = Phase::Failed(error);
            }
            return PollFix::Ready(Err(error));
        }
        match self.phase {
            Phase::Authorize => self.poll_authorization(&manager),
            Phase::Updating => self.take_fix(),
            Phase::Failed(error) => PollFix::Ready(Err(error)),
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(manager) = &self.manager {
            unsafe {
                if self.updates {
                    manager.stopUpdatingLocation();
                }
                manager.setDelegate(None);
            }
        }
        self.updates = false;
    }

    fn poll_authorization(&mut self, manager: &CLLocationManager) -> PollFix {
        let status = unsafe { manager.authorizationStatus() };
        match status {
            CLAuthorizationStatus::NotDetermined => {
                if !self.requested_auth {
                    unsafe { manager.requestWhenInUseAuthorization() };
                    self.requested_auth = true;
                }
                let waited = self.auth_started.unwrap_or_else(Instant::now).elapsed();
                if waited >= AUTH_WAIT {
                    self.phase = Phase::Failed(FixError::Unavailable);
                    PollFix::Ready(Err(FixError::Unavailable))
                } else {
                    PollFix::Pending
                }
            }
            CLAuthorizationStatus::Denied | CLAuthorizationStatus::Restricted => {
                self.phase = Phase::Failed(FixError::PermissionDenied);
                PollFix::Ready(Err(FixError::PermissionDenied))
            }
            CLAuthorizationStatus::AuthorizedAlways
            | CLAuthorizationStatus::AuthorizedWhenInUse => {
                if !self.updates {
                    unsafe { manager.startUpdatingLocation() };
                    self.updates = true;
                }
                self.phase = Phase::Updating;
                self.take_fix()
            }
            _ => {
                self.phase = Phase::Failed(FixError::Unavailable);
                PollFix::Ready(Err(FixError::Unavailable))
            }
        }
    }

    fn take_fix(&mut self) -> PollFix {
        let fix = self
            .slot
            .fixes
            .lock()
            .ok()
            .and_then(|mut fixes| fixes.pop());
        match fix {
            Some(fix) => PollFix::Ready(Ok(fix)),
            None => PollFix::Pending,
        }
    }

    fn apply_accuracy(&mut self, high_accuracy: bool) {
        if self.high_accuracy == Some(high_accuracy) {
            return;
        }
        let Some(manager) = &self.manager else {
            return;
        };
        unsafe {
            let accuracy = if high_accuracy {
                kCLLocationAccuracyBest
            } else {
                kCLLocationAccuracyThreeKilometers
            };
            manager.setDesiredAccuracy(accuracy);
            manager.setDistanceFilter(kCLDistanceFilterNone);
        }
        self.high_accuracy = Some(high_accuracy);
    }
}

fn pump(seconds: f64) {
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
    }
}

fn fix_from_location(location: &CLLocation) -> Option<Fix> {
    let coordinate = unsafe { location.coordinate() };
    let accuracy = unsafe { location.horizontalAccuracy() };
    if accuracy < 0.0
        || !coordinate.latitude.is_finite()
        || !coordinate.longitude.is_finite()
        || !(-90.0..=90.0).contains(&coordinate.latitude)
        || !(-180.0..=180.0).contains(&coordinate.longitude)
    {
        return None;
    }
    let vertical = unsafe { location.verticalAccuracy() };
    let altitude = unsafe { location.altitude() };
    let speed = unsafe { location.speed() };
    let heading = unsafe { location.course() };
    Some(Fix {
        latitude: coordinate.latitude,
        longitude: coordinate.longitude,
        accuracy,
        altitude: (vertical >= 0.0 && altitude.is_finite()).then_some(altitude),
        altitude_accuracy: (vertical >= 0.0 && vertical.is_finite()).then_some(vertical),
        heading: (heading >= 0.0 && heading < 360.0).then_some(heading),
        speed: (speed >= 0.0 && speed.is_finite()).then_some(speed),
    })
}
