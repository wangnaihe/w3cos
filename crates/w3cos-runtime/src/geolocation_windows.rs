//! Windows `Windows.Devices.Geolocation.Geolocator` adapter.
//!
//! `RequestAccessAsync` honors the Windows location privacy setting. Denial is
//! `PERMISSION_DENIED`. Location switched off, a missing radio, or an
//! unspecified access status is `POSITION_UNAVAILABLE`. `enableHighAccuracy`
//! selects `PositionAccuracy::High` or `Default`. One-shot reads use
//! `GetGeopositionAsync`; later watch updates come from `PositionChanged`.
//!
//! The provider thread may block inside the access prompt. Windows shows that
//! prompt for a foreground app; a headless process usually returns Denied or
//! Unspecified without a coordinate.

use std::sync::mpsc::{self, Receiver, Sender};

use windows::Devices::Geolocation::{
    GeolocationAccessStatus, Geolocator, PositionAccuracy, PositionChangedEventArgs, PositionStatus,
};
use windows::Foundation::TypedEventHandler;

use crate::geolocation_provider::{Fix, FixError, PollFix};

pub(crate) struct Session {
    locator: Option<Geolocator>,
    access_done: bool,
    fatal: Option<FixError>,
    high_accuracy: Option<bool>,
    updates: Option<UpdatePipe>,
    last_key: Option<(i64, i64, i64)>,
    primed: bool,
}

struct UpdatePipe {
    rx: Receiver<Result<Fix, FixError>>,
    token: Option<i64>,
    _tx: Sender<Result<Fix, FixError>>,
}

impl Session {
    pub(crate) fn open() -> Self {
        Self {
            locator: None,
            access_done: false,
            fatal: None,
            high_accuracy: None,
            updates: None,
            last_key: None,
            primed: false,
        }
    }

    pub(crate) fn poll(&mut self, high_accuracy: bool) -> PollFix {
        if let Some(error) = self.fatal {
            return PollFix::Ready(Err(error));
        }
        if let Err(error) = self.ensure(high_accuracy) {
            self.fatal = Some(error);
            return PollFix::Ready(Err(error));
        }
        self.drain_events();
        if !self.primed {
            match self.prime() {
                PollFix::Pending => return PollFix::Pending,
                PollFix::Ready(Err(FixError::Timeout)) => {
                    return PollFix::Ready(Err(FixError::Timeout));
                }
                PollFix::Ready(Err(error)) => {
                    self.fatal = Some(error);
                    return PollFix::Ready(Err(error));
                }
                PollFix::Ready(Ok(fix)) => {
                    self.primed = true;
                    self.remember(&fix);
                    return PollFix::Ready(Ok(fix));
                }
            }
        }
        self.drain_events()
    }

    pub(crate) fn shutdown(&mut self) {
        if let (Some(locator), Some(updates)) = (&self.locator, &self.updates) {
            if let Some(token) = updates.token {
                let _ = locator.RemovePositionChanged(token);
            }
        }
        self.updates = None;
    }

    fn ensure(&mut self, high_accuracy: bool) -> Result<(), FixError> {
        if !self.access_done {
            self.access_done = true;
            let status = Geolocator::RequestAccessAsync()
                .map_err(|err| map_error(&err))?
                .get()
                .map_err(|err| map_error(&err))?;
            match status {
                GeolocationAccessStatus::Allowed => {}
                GeolocationAccessStatus::Denied => return Err(FixError::PermissionDenied),
                _ => return Err(FixError::Unavailable),
            }
            let locator = Geolocator::new().map_err(|err| map_error(&err))?;
            match locator.LocationStatus() {
                Ok(PositionStatus::Disabled | PositionStatus::NotAvailable) => {
                    return Err(FixError::Unavailable);
                }
                _ => {}
            }
            self.locator = Some(locator);
            self.bind_updates()?;
        }
        self.apply_accuracy(high_accuracy)
    }

    fn apply_accuracy(&mut self, high_accuracy: bool) -> Result<(), FixError> {
        if self.high_accuracy == Some(high_accuracy) {
            return Ok(());
        }
        let locator = self.locator.as_ref().ok_or(FixError::Unavailable)?;
        let accuracy = if high_accuracy {
            PositionAccuracy::High
        } else {
            PositionAccuracy::Default
        };
        locator
            .SetDesiredAccuracy(accuracy)
            .map_err(|err| map_error(&err))?;
        self.high_accuracy = Some(high_accuracy);
        Ok(())
    }

    fn bind_updates(&mut self) -> Result<(), FixError> {
        let locator = self.locator.as_ref().ok_or(FixError::Unavailable)?;
        let (tx, rx) = mpsc::channel();
        let tx_event = tx.clone();
        let handler = TypedEventHandler::new(
            move |_sender: windows::core::Ref<'_, Geolocator>,
                  args: windows::core::Ref<'_, PositionChangedEventArgs>| {
                if let Some(args) = args.as_ref()
                    && let Ok(position) = args.Position()
                {
                    match fix_from_geoposition(&position) {
                        Ok(fix) => {
                            let _ = tx_event.send(Ok(fix));
                        }
                        Err(error) => {
                            let _ = tx_event.send(Err(error));
                        }
                    }
                }
                Ok(())
            },
        );
        let token = locator
            .PositionChanged(&handler)
            .map_err(|err| map_error(&err))?;
        self.updates = Some(UpdatePipe {
            rx,
            token: Some(token),
            _tx: tx,
        });
        Ok(())
    }

    fn prime(&mut self) -> PollFix {
        let Some(locator) = self.locator.clone() else {
            return PollFix::Ready(Err(FixError::Unavailable));
        };
        match locator.GetGeopositionAsync() {
            Ok(operation) => match operation.get() {
                Ok(position) => match fix_from_geoposition(&position) {
                    Ok(fix) => PollFix::Ready(Ok(fix)),
                    Err(error) => PollFix::Ready(Err(error)),
                },
                Err(err) => PollFix::Ready(Err(map_error(&err))),
            },
            Err(err) => PollFix::Ready(Err(map_error(&err))),
        }
    }

    fn drain_events(&mut self) -> PollFix {
        let Some(rx) = self.updates.as_ref().map(|updates| &updates.rx) else {
            return PollFix::Pending;
        };
        let mut newest: Option<Result<Fix, FixError>> = None;
        while let Ok(event) = rx.try_recv() {
            newest = Some(event);
        }
        match newest {
            Some(Ok(fix)) => {
                let key = key_of(&fix);
                if self.last_key == Some(key) {
                    PollFix::Pending
                } else {
                    self.remember(&fix);
                    PollFix::Ready(Ok(fix))
                }
            }
            Some(Err(error)) => PollFix::Ready(Err(error)),
            None => PollFix::Pending,
        }
    }

    fn remember(&mut self, fix: &Fix) {
        self.last_key = Some(key_of(fix));
    }
}

fn key_of(fix: &Fix) -> (i64, i64, i64) {
    (
        (fix.latitude * 1.0e6) as i64,
        (fix.longitude * 1.0e6) as i64,
        (fix.accuracy * 1.0e3) as i64,
    )
}

fn fix_from_geoposition(
    position: &windows::Devices::Geolocation::Geoposition,
) -> Result<Fix, FixError> {
    let coordinate = position.Coordinate().map_err(|err| map_error(&err))?;
    let point = coordinate.Point().map_err(|err| map_error(&err))?;
    let basic = point.Position().map_err(|err| map_error(&err))?;
    let accuracy = coordinate.Accuracy().unwrap_or(0.0);
    if !basic.Latitude.is_finite()
        || !basic.Longitude.is_finite()
        || !(-90.0..=90.0).contains(&basic.Latitude)
        || !(-180.0..=180.0).contains(&basic.Longitude)
        || !accuracy.is_finite()
        || accuracy < 0.0
    {
        return Err(FixError::Unavailable);
    }
    let altitude = optional_ref(coordinate.Altitude());
    let altitude_accuracy = optional_ref(coordinate.AltitudeAccuracy());
    let heading = optional_ref(coordinate.Heading());
    let speed = optional_ref(coordinate.Speed());
    Ok(Fix {
        latitude: basic.Latitude,
        longitude: basic.Longitude,
        accuracy,
        altitude: altitude.filter(|value| value.is_finite()),
        altitude_accuracy: altitude_accuracy.filter(|value| value.is_finite() && *value >= 0.0),
        heading: heading.filter(|value| value.is_finite() && (0.0..360.0).contains(value)),
        speed: speed.filter(|value| value.is_finite() && *value >= 0.0),
    })
}

fn optional_ref(value: windows::core::Result<windows::Foundation::IReference<f64>>) -> Option<f64> {
    value.ok().and_then(|reference| reference.Value().ok())
}

fn map_error(err: &windows::core::Error) -> FixError {
    let code = err.code().0 as u32;
    // E_ACCESSDENIED, ERROR_TIMEOUT, WAIT_TIMEOUT.
    if code == 0x8007_0005 {
        return FixError::PermissionDenied;
    }
    if code == 0x8007_05B4 || code == 0x8007_0102 {
        return FixError::Timeout;
    }
    let message = err.to_string().to_ascii_lowercase();
    if message.contains("access is denied") || message.contains("denied") {
        return FixError::PermissionDenied;
    }
    if message.contains("timeout") || message.contains("timed out") {
        return FixError::Timeout;
    }
    FixError::Unavailable
}
