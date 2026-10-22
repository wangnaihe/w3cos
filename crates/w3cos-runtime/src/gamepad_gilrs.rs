//! Desktop gamepad backend.
//!
//! `gilrs` owns the platform handles on one thread. The JavaScript thread only
//! reads published snapshots and posts rumble commands. Linux links libudev;
//! a missing library fails `Gilrs::new` and the Gamepad API reports that
//! instead of inventing controllers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use gilrs::ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder, Repeat, Replay, Ticks};
use gilrs::{Axis, Button, GamepadId, Gilrs, MappingSource};

use crate::gamepad_web::{AxisRole, ButtonRole, PlatformSample, SampleAxis, SampleButton};

struct Shared {
    samples: Mutex<Vec<PlatformSample>>,
    failures: Mutex<Vec<String>>,
    error: Mutex<Option<String>>,
    tx: Mutex<Option<Sender<Command>>>,
    ready: AtomicBool,
}

enum Command {
    Play {
        device_key: String,
        strong: f32,
        weak: f32,
        delay_ms: u32,
        duration_ms: u32,
    },
    Reset {
        device_key: String,
    },
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        samples: Mutex::new(Vec::new()),
        failures: Mutex::new(Vec::new()),
        error: Mutex::new(None),
        tx: Mutex::new(None),
        ready: AtomicBool::new(false),
    })
}

pub(crate) fn native_enabled() -> bool {
    !cfg!(test) || std::env::var_os("W3COS_GAMEPAD_DEVICE_TESTS").is_some()
}

pub(crate) fn backend_name() -> &'static str {
    if !native_enabled() {
        return "host";
    }
    if last_error().is_some() {
        return "gilrs-unavailable";
    }
    "gilrs"
}

pub(crate) fn last_error() -> Option<String> {
    if !native_enabled() {
        return None;
    }
    shared().error.lock().ok().and_then(|slot| slot.clone())
}

pub(crate) fn samples() -> Vec<PlatformSample> {
    if !native_enabled() {
        return Vec::new();
    }
    start();
    if !shared().ready.load(Ordering::Acquire) {
        let deadline = Instant::now() + Duration::from_millis(500);
        while !shared().ready.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }
    shared()
        .samples
        .lock()
        .map(|slot| slot.clone())
        .unwrap_or_else(|poison| poison.into_inner().clone())
}

pub(crate) fn take_haptic_failures() -> Vec<String> {
    if !native_enabled() {
        return Vec::new();
    }
    shared()
        .failures
        .lock()
        .map(|mut slot| std::mem::take(&mut *slot))
        .unwrap_or_else(|poison| std::mem::take(&mut *poison.into_inner()))
}

pub(crate) fn play(
    device_key: &str,
    strong: f32,
    weak: f32,
    delay_ms: u32,
    duration_ms: u32,
) -> Result<(), String> {
    send(Command::Play {
        device_key: device_key.to_string(),
        strong,
        weak,
        delay_ms,
        duration_ms,
    })
}

pub(crate) fn reset(device_key: &str) {
    let _ = send(Command::Reset {
        device_key: device_key.to_string(),
    });
}

fn send(command: Command) -> Result<(), String> {
    if !native_enabled() {
        return Err("gamepad adapter is not enabled".into());
    }
    start();
    let sender = shared()
        .tx
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .ok_or_else(|| "gamepad thread is not running".to_string())?;
    sender
        .send(command)
        .map_err(|_| "gamepad thread stopped".to_string())
}

fn start() {
    static START: Once = Once::new();
    START.call_once(|| {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut slot) = shared().tx.lock() {
            *slot = Some(tx);
        }
        if thread::Builder::new()
            .name("w3cos-gamepad".into())
            .spawn(move || run(rx))
            .is_err()
        {
            if let Ok(mut slot) = shared().error.lock() {
                *slot = Some("failed to start the gamepad thread".into());
            }
            shared().ready.store(true, Ordering::Release);
        }
    });
}

fn run(rx: Receiver<Command>) {
    let mut gilrs = match Gilrs::new() {
        Ok(gilrs) => gilrs,
        Err(err) => {
            if let Ok(mut slot) = shared().error.lock() {
                *slot = Some(err.to_string());
            }
            shared().ready.store(true, Ordering::Release);
            return;
        }
    };
    let mut effects: HashMap<String, Effect> = HashMap::new();
    let mut previous_keys = Vec::new();
    let mut ids = HashMap::new();
    publish(&mut gilrs, &mut ids, &mut previous_keys);
    shared().ready.store(true, Ordering::Release);
    loop {
        while gilrs.next_event().is_some() {}
        publish(&mut gilrs, &mut ids, &mut previous_keys);
        while let Ok(command) = rx.try_recv() {
            match command {
                Command::Play {
                    device_key,
                    strong,
                    weak,
                    delay_ms,
                    duration_ms,
                } => play_effect(
                    &mut gilrs,
                    &mut effects,
                    &ids,
                    PlayRequest {
                        device_key: &device_key,
                        strong,
                        weak,
                        delay_ms,
                        duration_ms,
                    },
                ),
                Command::Reset { device_key } => {
                    if let Some(effect) = effects.remove(&device_key) {
                        let _ = effect.stop();
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
}

fn publish(
    gilrs: &mut Gilrs,
    ids: &mut HashMap<String, GamepadId>,
    previous_keys: &mut Vec<String>,
) {
    ids.clear();
    let mut samples = Vec::new();
    for (id, pad) in gilrs.gamepads() {
        let sample = sample_of(&pad);
        ids.insert(sample.device_key.clone(), id);
        samples.push(sample);
    }
    let mut keys: Vec<String> = samples
        .iter()
        .map(|sample| sample.device_key.clone())
        .collect();
    keys.sort();
    let changed = keys != *previous_keys;
    *previous_keys = keys;
    match shared().samples.lock() {
        Ok(mut slot) => *slot = samples,
        Err(poison) => *poison.into_inner() = samples,
    }
    if changed {
        crate::uitest::poke_event_loop();
    }
}

struct PlayRequest<'a> {
    device_key: &'a str,
    strong: f32,
    weak: f32,
    delay_ms: u32,
    duration_ms: u32,
}

fn play_effect(
    gilrs: &mut Gilrs,
    effects: &mut HashMap<String, Effect>,
    ids: &HashMap<String, GamepadId>,
    request: PlayRequest<'_>,
) {
    let PlayRequest {
        device_key,
        strong,
        weak,
        delay_ms,
        duration_ms,
    } = request;
    if let Some(effect) = effects.remove(device_key) {
        let _ = effect.stop();
    }
    let Some(id) = ids.get(device_key).copied() else {
        note_failure(device_key);
        return;
    };
    if duration_ms == 0 {
        return;
    }
    let magnitude =
        |value: f32| -> u16 { (value.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16 };
    let play_for = Ticks::from_ms(duration_ms.max(1));
    let after = Ticks::from_ms(delay_ms);
    let mut builder = EffectBuilder::new();
    builder
        .add_effect(BaseEffect {
            kind: BaseEffectType::Strong {
                magnitude: magnitude(strong),
            },
            scheduling: Replay {
                after,
                play_for,
                with_delay: Ticks::from_ms(0),
            },
            ..Default::default()
        })
        .add_effect(BaseEffect {
            kind: BaseEffectType::Weak {
                magnitude: magnitude(weak),
            },
            scheduling: Replay {
                after,
                play_for,
                with_delay: Ticks::from_ms(0),
            },
            ..Default::default()
        })
        .repeat(Repeat::For(Ticks::from_ms(
            delay_ms.saturating_add(duration_ms),
        )))
        .gamepads(&[id]);
    match builder.finish(gilrs) {
        Ok(effect) => match effect.play() {
            Ok(()) => {
                effects.insert(device_key.to_string(), effect);
            }
            Err(_) => note_failure(device_key),
        },
        Err(_) => note_failure(device_key),
    }
}

fn note_failure(device_key: &str) {
    if let Ok(mut slot) = shared().failures.lock() {
        slot.push(device_key.to_string());
    }
}

fn sample_of(pad: &gilrs::Gamepad<'_>) -> PlatformSample {
    let standard_layout = !matches!(pad.mapping_source(), MappingSource::None);
    let mut buttons = Vec::new();
    let mut axes = Vec::new();
    for (button, role) in BUTTONS {
        push_button(pad, *button, *role, &mut buttons, !standard_layout);
    }
    for (axis, role) in AXES {
        // gilrs stick Y is negative up / positive down, the same sign as the
        // W3C standard vertical axes, so the value is copied through.
        push_axis(pad, *axis, *role, &mut axes, !standard_layout);
    }
    PlatformSample {
        device_key: device_key(pad),
        id: gamepad_id(pad),
        standard_layout,
        ff_supported: pad.is_ff_supported(),
        buttons,
        axes,
    }
}

const BUTTONS: &[(Button, ButtonRole)] = &[
    (Button::South, ButtonRole::South),
    (Button::East, ButtonRole::East),
    (Button::West, ButtonRole::West),
    (Button::North, ButtonRole::North),
    (Button::C, ButtonRole::Other),
    (Button::Z, ButtonRole::Other),
    (Button::LeftTrigger, ButtonRole::LeftBumper),
    (Button::RightTrigger, ButtonRole::RightBumper),
    (Button::LeftTrigger2, ButtonRole::LeftTrigger),
    (Button::RightTrigger2, ButtonRole::RightTrigger),
    (Button::Select, ButtonRole::Select),
    (Button::Start, ButtonRole::Start),
    (Button::Mode, ButtonRole::Guide),
    (Button::LeftThumb, ButtonRole::LeftStick),
    (Button::RightThumb, ButtonRole::RightStick),
    (Button::DPadUp, ButtonRole::DpadUp),
    (Button::DPadDown, ButtonRole::DpadDown),
    (Button::DPadLeft, ButtonRole::DpadLeft),
    (Button::DPadRight, ButtonRole::DpadRight),
];

const AXES: &[(Axis, AxisRole)] = &[
    (Axis::LeftStickX, AxisRole::LeftX),
    (Axis::LeftStickY, AxisRole::LeftY),
    (Axis::RightStickX, AxisRole::RightX),
    (Axis::RightStickY, AxisRole::RightY),
    (Axis::LeftZ, AxisRole::Other),
    (Axis::RightZ, AxisRole::Other),
    (Axis::DPadX, AxisRole::DpadX),
    (Axis::DPadY, AxisRole::DpadY),
];

fn push_button(
    pad: &gilrs::Gamepad<'_>,
    button: Button,
    role: ButtonRole,
    out: &mut Vec<SampleButton>,
    require_code: bool,
) {
    if require_code && pad.button_code(button).is_none() {
        return;
    }
    if !require_code && matches!(role, ButtonRole::Other) {
        return;
    }
    let (value, pressed) = pad
        .button_data(button)
        .map(|data| (f64::from(data.value()), data.is_pressed()))
        .unwrap_or((0.0, false));
    out.push(SampleButton {
        role,
        value,
        pressed,
    });
}

fn push_axis(
    pad: &gilrs::Gamepad<'_>,
    axis: Axis,
    role: AxisRole,
    out: &mut Vec<SampleAxis>,
    require_code: bool,
) {
    if require_code && pad.axis_code(axis).is_none() {
        return;
    }
    if !require_code && matches!(role, AxisRole::Other) {
        return;
    }
    let value = pad
        .axis_data(axis)
        .map(|data| f64::from(data.value()))
        .unwrap_or(0.0);
    out.push(SampleAxis { role, value });
}

fn device_key(pad: &gilrs::Gamepad<'_>) -> String {
    let uuid = pad.uuid();
    if uuid.iter().any(|byte| *byte != 0) {
        return uuid.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    format!("gilrs:{}:{}", usize::from(pad.id()), pad.name())
}

fn gamepad_id(pad: &gilrs::Gamepad<'_>) -> String {
    let name = pad.name().trim();
    let name = if name.is_empty() { "Gamepad" } else { name };
    match (pad.vendor_id(), pad.product_id()) {
        (Some(vendor), Some(product)) => format!("{name} ({vendor:04x}:{product:04x})"),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_poll_is_opt_in() {
        if std::env::var_os("W3COS_GAMEPAD_DEVICE_TESTS").is_none() {
            assert!(samples().is_empty());
            assert_eq!(backend_name(), "host");
            return;
        }
        let connected = samples();
        assert_eq!(
            backend_name(),
            "gilrs",
            "{}",
            last_error().unwrap_or_default()
        );
        for sample in connected {
            assert!(!sample.device_key.is_empty());
            assert!(!sample.id.is_empty());
        }
    }
}
