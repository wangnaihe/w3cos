//! macOS AVFoundation and Windows Media Foundation capture through `nokhwa`.
//!
//! macOS permission uses `AVAuthorizationStatus` from `nokhwa-bindings-macos`.
//! `NotDetermined` requests access and waits for the system prompt. Denied or
//! restricted access rejects `NotAllowedError`. The host application's
//! `Info.plist` must include `NSCameraUsageDescription`; without that key
//! macOS will not grant camera access (the prompt is skipped or the process
//! is not allowed to capture).
//!
//! Frames are decoded to RGBA on the capture thread. The thread owns the
//! `nokhwa` camera, which is not moved across threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{ApiBackend, CameraFormat, CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::{Camera, NokhwaError, query};

use super::{
    Backend, CameraDevice, CameraError, Facing, FrameHub, Negotiated, VideoMode, VideoPrefs,
};

pub(super) fn list() -> Vec<CameraDevice> {
    let backend = if cfg!(target_os = "macos") {
        ApiBackend::AVFoundation
    } else {
        ApiBackend::MediaFoundation
    };
    let Ok(infos) = query(backend) else {
        return Vec::new();
    };
    infos
        .into_iter()
        .enumerate()
        .map(|(fallback, info)| {
            let misc = info.misc();
            let description = info.description().to_string();
            let prefix = if cfg!(target_os = "macos") {
                "avf"
            } else {
                "mf"
            };
            let index = match info.index() {
                CameraIndex::Index(index) => *index,
                CameraIndex::String(_) => fallback as u32,
            };
            CameraDevice {
                label: {
                    let name = info.human_name();
                    if name.is_empty() {
                        format!("Camera {index}")
                    } else {
                        name
                    }
                },
                device_key: format!("{prefix}:{misc}"),
                group_key: format!("{prefix}-group:{misc}"),
                path: std::path::PathBuf::from(&misc),
                facing: facing_from_description(&description),
                modes: Vec::new(),
                index,
                backend: if cfg!(target_os = "macos") {
                    Backend::AvFoundation
                } else {
                    Backend::MediaFoundation
                },
            }
        })
        .collect()
}

pub(super) fn ensure_permission() -> Result<(), CameraError> {
    #[cfg(target_os = "macos")]
    {
        use nokhwa_bindings_macos::{
            AVAuthorizationStatus, current_authorization_status, request_permission_with_callback,
        };
        return match current_authorization_status() {
            AVAuthorizationStatus::Authorized => Ok(()),
            AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => {
                Err(CameraError::not_allowed(super::PERMISSION_DENIED))
            }
            AVAuthorizationStatus::NotDetermined => {
                let (tx, rx) = std::sync::mpsc::channel();
                request_permission_with_callback(move |granted| {
                    let _ = tx.send(granted);
                });
                match rx.recv_timeout(Duration::from_secs(60)) {
                    Ok(true) => Ok(()),
                    _ => Err(CameraError::not_allowed(super::PERMISSION_DENIED)),
                }
            }
        };
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

pub(super) fn capture_loop(
    device: CameraDevice,
    prefs: VideoPrefs,
    stop: Arc<AtomicBool>,
    hub: Arc<FrameHub>,
    ready: Sender<Result<Negotiated, CameraError>>,
) {
    let (mut camera, modes) = match open_device(&device, &prefs) {
        Ok(opened) => opened,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let negotiated = Negotiated {
        width: camera.resolution().width(),
        height: camera.resolution().height(),
        frame_rate: (camera.frame_rate() > 0).then(|| f64::from(camera.frame_rate())),
        facing: device.facing,
        modes,
    };
    if let Err(error) = super::verify_negotiated(
        &prefs,
        negotiated.width,
        negotiated.height,
        negotiated.frame_rate,
    ) {
        let _ = camera.stop_stream();
        let _ = ready.send(Err(error));
        return;
    }
    if let Err(error) = publish_one(&mut camera, &hub) {
        let _ = camera.stop_stream();
        let _ = ready.send(Err(error));
        return;
    }
    let _ = ready.send(Ok(negotiated));
    while !stop.load(Ordering::Acquire) {
        if publish_one(&mut camera, &hub).is_err() {
            break;
        }
    }
    let _ = camera.stop_stream();
}

fn open_device(
    device: &CameraDevice,
    prefs: &VideoPrefs,
) -> Result<(Camera, Vec<VideoMode>), CameraError> {
    let index = if device.path.as_os_str().is_empty() {
        CameraIndex::Index(device.index)
    } else {
        CameraIndex::String(device.path.to_string_lossy().into_owned())
    };
    let mut camera = Camera::new(
        index,
        RequestedFormat::new::<RgbAFormat>(RequestedFormatType::None),
    )
    .map_err(map_nokhwa)?;
    let formats = camera.compatible_camera_formats().unwrap_or_default();
    let modes = if formats.is_empty() {
        let current = camera.camera_format();
        vec![VideoMode::discrete(
            current.width(),
            current.height(),
            f64::from(current.frame_rate()),
        )]
    } else {
        formats
            .iter()
            .map(|format| {
                VideoMode::discrete(
                    format.width(),
                    format.height(),
                    f64::from(format.frame_rate()),
                )
            })
            .collect()
    };
    let chosen = match super::select_mode(&modes, device.facing, prefs) {
        Ok(chosen) => chosen,
        Err(error) => {
            let _ = camera.stop_stream();
            return Err(error);
        }
    };
    let selected = formats.iter().copied().find(|format| {
        format.width() == chosen.width
            && format.height() == chosen.height
            && chosen
                .frame_rate
                .is_none_or(|fps| (f64::from(format.frame_rate()) - fps).abs() < 1.0)
    });
    if let Some(format) = selected.or_else(|| nearest_format(&formats, chosen.width, chosen.height))
    {
        if let Err(error) = camera.set_camera_format(format) {
            let _ = camera.stop_stream();
            return Err(map_nokhwa(error));
        }
    }
    if let Err(error) = camera.open_stream() {
        let _ = camera.stop_stream();
        return Err(map_nokhwa(error));
    }
    Ok((camera, modes))
}

fn nearest_format(formats: &[CameraFormat], width: u32, height: u32) -> Option<CameraFormat> {
    formats
        .iter()
        .copied()
        .min_by_key(|format| format.width().abs_diff(width) + format.height().abs_diff(height))
}

fn publish_one(camera: &mut Camera, hub: &FrameHub) -> Result<(), CameraError> {
    let buffer = camera.frame().map_err(map_nokhwa)?;
    let image = buffer.decode_image::<RgbAFormat>().map_err(map_nokhwa)?;
    let (width, height) = image.dimensions();
    hub.publish(width, height, image.into_raw(), None);
    Ok(())
}

fn facing_from_description(description: &str) -> Facing {
    if description.contains("Front") {
        Facing::User
    } else if description.contains("Back") {
        Facing::Environment
    } else {
        Facing::Unknown
    }
}

fn map_nokhwa(error: NokhwaError) -> CameraError {
    let text = error.to_string();
    let lower = text.to_lowercase();
    if lower.contains("permission")
        || lower.contains("not authorized")
        || lower.contains("denied")
        || lower.contains("restricted")
    {
        CameraError::not_allowed(format!(
            "{text}; on macOS the host Info.plist must include NSCameraUsageDescription"
        ))
    } else {
        CameraError::not_readable(text)
    }
}
