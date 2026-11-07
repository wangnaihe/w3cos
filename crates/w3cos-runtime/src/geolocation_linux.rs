//! Linux GeoClue2 client on the system bus.
//!
//! GeoClue2 requires a desktop id before `Start`. This client registers
//! `w3cos` (the desktop-file basename, without `.desktop`) and maps
//! `enableHighAccuracy` onto `GClueAccuracyLevel`: city (`4`) or exact (`8`).
//!
//! A config allow-list entry, when the agent is not running, looks like:
//!
//! ```text
//! [w3cos]
//! allowed=true
//! system=false
//! users=
//! ```
//!
//! A missing system bus or `org.freedesktop.GeoClue2` service is
//! position-unavailable. An agent or config denial is permission-denied.
//! Coordinates are read from the location object; nothing here is synthesized.

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

use crate::geolocation_provider::{Fix, FixError, PollFix};

/// Desktop-file id GeoClue matches against `DesktopId` and geoclue.conf.
pub(crate) const DESKTOP_ID: &str = "w3cos";

const SERVICE: &str = "org.freedesktop.GeoClue2";
const MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const MANAGER_IFACE: &str = "org.freedesktop.GeoClue2.Manager";
const CLIENT_IFACE: &str = "org.freedesktop.GeoClue2.Client";
const LOCATION_IFACE: &str = "org.freedesktop.GeoClue2.Location";

/// `GClueAccuracyLevel` values. City is the coarse hint; exact asks for GPS.
pub(crate) fn accuracy_level(high_accuracy: bool) -> u32 {
    if high_accuracy { 8 } else { 4 }
}

pub(crate) struct Session {
    conn: Option<Connection>,
    client_path: Option<OwnedObjectPath>,
    started: bool,
    accuracy: Option<u32>,
    last_key: Option<(i64, i64, i64)>,
    fatal: Option<FixError>,
}

impl Session {
    pub(crate) fn open() -> Self {
        Self {
            conn: None,
            client_path: None,
            started: false,
            accuracy: None,
            last_key: None,
            fatal: None,
        }
    }

    pub(crate) fn poll(&mut self, high_accuracy: bool) -> PollFix {
        if let Some(error) = self.fatal {
            return PollFix::Ready(Err(error));
        }
        if let Err(error) = self.ensure(high_accuracy) {
            if matches!(error, FixError::PermissionDenied | FixError::Unavailable) {
                self.fatal = Some(error);
            }
            return PollFix::Ready(Err(error));
        }
        match self.read_fix() {
            Err(error) => {
                self.fatal = Some(error);
                PollFix::Ready(Err(error))
            }
            Ok(None) => PollFix::Pending,
            Ok(Some(fix)) => {
                let key = (
                    (fix.latitude * 1.0e6) as i64,
                    (fix.longitude * 1.0e6) as i64,
                    (fix.accuracy * 1.0e3) as i64,
                );
                if self.last_key == Some(key) {
                    PollFix::Pending
                } else {
                    self.last_key = Some(key);
                    PollFix::Ready(Ok(fix))
                }
            }
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if self.started {
            if let Some(proxy) = self.client_proxy() {
                let _ = proxy.call_noreply("Stop", &());
            }
        }
        self.started = false;
    }

    fn ensure(&mut self, high_accuracy: bool) -> Result<(), FixError> {
        if self.conn.is_none() {
            match Connection::system() {
                Ok(conn) => self.conn = Some(conn),
                Err(err) => {
                    warn_once(&err.to_string());
                    return Err(FixError::Unavailable);
                }
            }
        }
        if self.client_path.is_none() {
            let conn = self.conn.as_ref().expect("geoclue connection");
            let manager =
                Proxy::new(conn, SERVICE, MANAGER_PATH, MANAGER_IFACE).map_err(|err| {
                    warn_once(&err.to_string());
                    FixError::Unavailable
                })?;
            let path: OwnedObjectPath = manager.call("GetClient", &()).map_err(map_error)?;
            self.client_path = Some(path);
        }
        let level = accuracy_level(high_accuracy);
        if self.accuracy != Some(level) {
            {
                let proxy = self.client_proxy().ok_or(FixError::Unavailable)?;
                proxy
                    .set_property("DesktopId", DESKTOP_ID)
                    .map_err(map_error)?;
                proxy
                    .set_property("RequestedAccuracyLevel", level)
                    .map_err(map_error)?;
                let _ = proxy.set_property("DistanceThreshold", 0u32);
                let _ = proxy.set_property("TimeThreshold", 0u32);
            }
            self.accuracy = Some(level);
        }
        if !self.started {
            {
                let proxy = self.client_proxy().ok_or(FixError::Unavailable)?;
                proxy.call_noreply("Start", &()).map_err(map_error)?;
            }
            self.started = true;
        }
        Ok(())
    }

    fn client_proxy(&self) -> Option<Proxy<'_>> {
        let conn = self.conn.as_ref()?;
        let path = self.client_path.as_ref()?;
        Proxy::new(conn, SERVICE, path.as_str(), CLIENT_IFACE).ok()
    }

    fn read_fix(&self) -> Result<Option<Fix>, FixError> {
        let proxy = self.client_proxy().ok_or(FixError::Unavailable)?;
        let path: OwnedObjectPath = proxy.get_property("Location").map_err(map_error)?;
        let path = path.as_str();
        if path.is_empty() || path == "/" {
            return Ok(None);
        }
        let conn = self.conn.as_ref().ok_or(FixError::Unavailable)?;
        let location = Proxy::new(conn, SERVICE, path, LOCATION_IFACE).map_err(map_error)?;
        let latitude: f64 = location.get_property("Latitude").map_err(map_error)?;
        let longitude: f64 = location.get_property("Longitude").map_err(map_error)?;
        let accuracy: f64 = location.get_property("Accuracy").map_err(map_error)?;
        let altitude: f64 = location.get_property("Altitude").unwrap_or(f64::NAN);
        let speed: f64 = location.get_property("Speed").unwrap_or(-1.0);
        let heading: f64 = location.get_property("Heading").unwrap_or(-1.0);
        Ok(Some(reading_from_geoclue(
            latitude, longitude, accuracy, altitude, speed, heading,
        )?))
    }
}

pub(crate) fn reading_from_geoclue(
    latitude: f64,
    longitude: f64,
    accuracy: f64,
    altitude: f64,
    speed: f64,
    heading: f64,
) -> Result<Fix, FixError> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
        || !accuracy.is_finite()
        || accuracy < 0.0
    {
        return Err(FixError::Unavailable);
    }
    Ok(Fix {
        latitude,
        longitude,
        accuracy,
        // GeoClue uses -G_MAXDOUBLE when altitude was not measured.
        altitude: (altitude.is_finite() && altitude > -1.0e300 && altitude < 1.0e300)
            .then_some(altitude),
        altitude_accuracy: None,
        speed: (speed.is_finite() && speed >= 0.0).then_some(speed),
        heading: (heading.is_finite() && (0.0..360.0).contains(&heading)).then_some(heading),
    })
}

fn map_error(err: impl std::fmt::Display) -> FixError {
    let text = err.to_string();
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("accessdenied")
        || lowered.contains("not allowed")
        || lowered.contains("denied")
    {
        warn_once(&text);
        return FixError::PermissionDenied;
    }
    warn_once(&text);
    FixError::Unavailable
}

fn warn_once(detail: &str) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let detail = detail.to_string();
    ONCE.call_once(move || {
        eprintln!(
            "[w3cos] warning: Geolocation could not use GeoClue2 ({detail}). \
             DesktopId is `{DESKTOP_ID}`; allow it in /etc/geoclue/geoclue.conf when no agent is running."
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_id_and_accuracy_levels_follow_geoclue() {
        assert_eq!(DESKTOP_ID, "w3cos");
        assert_eq!(accuracy_level(false), 4);
        assert_eq!(accuracy_level(true), 8);
    }

    #[test]
    fn unknown_geoclue_fields_stay_null_and_bad_coordinates_fail() {
        let fix = reading_from_geoclue(31.2, 121.5, 40.0, -1.0e308, -1.0, -1.0).unwrap();
        assert!(fix.altitude.is_none());
        assert!(fix.speed.is_none());
        assert!(fix.heading.is_none());
        assert!(reading_from_geoclue(f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0).is_err());
        assert!(reading_from_geoclue(0.0, 0.0, -5.0, 0.0, 0.0, 0.0).is_err());
    }
}
