//! Gamepad API.
//!
//! Desktop builds with `web-gamepad` poll `gilrs` on a side thread and apply
//! snapshots on the event-loop tick. A recognized controller uses the W3C
//! `standard` mapping (17 buttons, 4 axes). Anything gilrs does not recognize
//! keeps mapping `""` and the reported control order. `dual-rumble` is sent
//! through gilrs force feedback when the device supports it.
//!
//! Android, iOS and OHOS keep host-injectable snapshots. Their actuators still
//! resolve a compatible `"complete"` and warn once.
//!
//! The spec's `gamepad` permissions policy defaults to allow (`*`). This
//! runtime does not parse `Permissions-Policy` headers. `FeaturePolicy` is a
//! compatibility stub and is not consulted. Hosts that model the feature call
//! [`set_gamepad_permissions_policy`]; a refusal throws `SecurityError` and
//! suppresses connect/disconnect events.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GamepadButtonState {
    pub pressed: bool,
    pub touched: bool,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GamepadState {
    pub id: String,
    pub index: u32,
    pub connected: bool,
    pub timestamp: f64,
    pub mapping: String,
    pub axes: Vec<f64>,
    pub buttons: Vec<GamepadButtonState>,
}

/// Control role before W3C remapping. `Other` is only present for unrecognized
/// layouts and is emitted in backend order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonRole {
    South,
    East,
    West,
    North,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    LeftStick,
    RightStick,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Guide,
    Other,
}

/// Stick Y is negative up and positive down, matching the W3C standard axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisRole {
    LeftX,
    LeftY,
    RightX,
    RightY,
    DpadX,
    DpadY,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampleButton {
    pub role: ButtonRole,
    pub value: f64,
    pub pressed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampleAxis {
    pub role: AxisRole,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlatformSample {
    pub device_key: String,
    pub id: String,
    pub standard_layout: bool,
    pub ff_supported: bool,
    pub buttons: Vec<SampleButton>,
    pub axes: Vec<SampleAxis>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordedRumble {
    pub device_key: String,
    pub strong: f64,
    pub weak: f64,
    pub delay_ms: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct MockGamepadBackend {
    pub pads: Vec<PlatformSample>,
    pub plays: Vec<RecordedRumble>,
    pub resets: Vec<String>,
    pub fail_play: bool,
}

const STANDARD_BUTTONS: [ButtonRole; 17] = [
    ButtonRole::South,
    ButtonRole::East,
    ButtonRole::West,
    ButtonRole::North,
    ButtonRole::LeftBumper,
    ButtonRole::RightBumper,
    ButtonRole::LeftTrigger,
    ButtonRole::RightTrigger,
    ButtonRole::Select,
    ButtonRole::Start,
    ButtonRole::LeftStick,
    ButtonRole::RightStick,
    ButtonRole::DpadUp,
    ButtonRole::DpadDown,
    ButtonRole::DpadLeft,
    ButtonRole::DpadRight,
    ButtonRole::Guide,
];

const AXIS_GESTURE: f64 = 0.4;
const MAX_EFFECT_MS: f64 = 5_000.0;

#[derive(Clone, Debug)]
enum HapticMode {
    Compat,
    Platform { device_key: String, ff: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PadSource {
    Host,
    Platform,
}

struct PadRecord {
    device_key: String,
    source: PadSource,
    ff_supported: bool,
    exposed: bool,
    saw_neutral: bool,
    index: Option<u32>,
    state: GamepadState,
    object: Option<Value>,
}

struct Playing {
    device_key: String,
    resolve: Value,
    reject: Value,
    deadline: f64,
}

struct RumbleParams {
    duration_ms: u64,
    delay_ms: u64,
    strong: f64,
    weak: f64,
}

thread_local! {
    static RECORDS: RefCell<HashMap<String, PadRecord>> = RefCell::new(HashMap::new());
    static MOCK: RefCell<Option<MockGamepadBackend>> = const { RefCell::new(None) };
    static PLAYING: RefCell<Vec<Playing>> = const { RefCell::new(Vec::new()) };
    static POLICY_ALLOWED: Cell<bool> = const { Cell::new(true) };
    static HAS_GESTURE: Cell<bool> = const { Cell::new(false) };
    static POLLING: Cell<bool> = const { Cell::new(false) };
    static EVENTS: Cell<usize> = const { Cell::new(0) };
    static GAMEPAD_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static GAMEPAD_BUTTON_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static GAMEPAD_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static GAMEPAD_HAPTIC_ACTUATOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

struct PollFlag;

impl Drop for PollFlag {
    fn drop(&mut self) {
        POLLING.with(|flag| flag.set(false));
    }
}

fn illegal_class(name: &str, parent: Option<Value>) -> Value {
    let class_name = name.to_string();
    let class = Value::function(move |_, _| {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".into(), Value::string("TypeError")),
            (
                "message".into(),
                Value::string(&format!("Illegal constructor: {class_name}")),
            ),
        ])))
    });
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    if let Some(parent) = parent {
        w3cos_core::class::set_prototype_of(&prototype, &parent.get_property("prototype"));
    }
    class.set_property("prototype", prototype);
    class
}

pub fn gamepad_class() -> Value {
    GAMEPAD_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class("Gamepad", None);
        for property in [
            "axes",
            "buttons",
            "connected",
            "id",
            "index",
            "mapping",
            "timestamp",
            "vibrationActuator",
        ] {
            class
                .get_property("prototype")
                .set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn gamepad_button_class() -> Value {
    GAMEPAD_BUTTON_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class("GamepadButton", None);
        for property in ["pressed", "touched", "value"] {
            class
                .get_property("prototype")
                .set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn gamepad_event_class() -> Value {
    GAMEPAD_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property("gamepad", init.get_property("gamepad"));
            Value::Undefined
        });
        class.set_property("name", Value::string("GamepadEvent"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("gamepad", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn gamepad_haptic_actuator_class() -> Value {
    GAMEPAD_HAPTIC_ACTUATOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class("GamepadHapticActuator", None);
        for property in ["effects", "playEffect", "reset", "type"] {
            class
                .get_property("prototype")
                .set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

/// Refuse or restore the `gamepad` permissions policy. The default is allow.
pub fn set_gamepad_permissions_policy(allowed: bool) {
    POLICY_ALLOWED.with(|flag| flag.set(allowed));
    if allowed {
        let _ = poll();
    }
}

pub fn set_mock_gamepad_backend(backend: Option<MockGamepadBackend>) {
    MOCK.with(|slot| *slot.borrow_mut() = backend);
}

pub fn mock_gamepad_backend_mut<R>(f: impl FnOnce(&mut MockGamepadBackend) -> R) -> Option<R> {
    MOCK.with(|slot| slot.borrow_mut().as_mut().map(f))
}

pub fn platform_backend_name() -> &'static str {
    if MOCK.with(|slot| slot.borrow().is_some()) {
        return "mock";
    }
    desktop_backend_name()
}

pub(crate) fn adapter_error() -> String {
    desktop_error()
}

/// Apply platform samples, fire connection events, and settle haptics.
pub fn poll() -> usize {
    if POLLING.with(Cell::get) {
        return 0;
    }
    POLLING.with(|flag| flag.set(true));
    let _guard = PollFlag;
    EVENTS.with(|count| count.set(0));
    sync_platform();
    settle_haptics();
    expose_pending();
    EVENTS.with(Cell::get)
}

pub fn on_document_visibility(state: &str) {
    if state == "hidden" {
        stop_all_playing();
    }
}

/// Insert, update, or disconnect a host-injected controller.
pub fn update_gamepad(state: GamepadState) {
    let key = host_key(state.index);
    if !state.connected {
        disconnect(&key, Some(state.timestamp.max(0.0)));
        return;
    }
    let refreshed = RECORDS.with(|records| {
        let mut records = records.borrow_mut();
        if let Some(record) = records.get_mut(&key) {
            record.state = state.clone();
            record.state.connected = true;
            record.index = Some(state.index);
            record.exposed.then(|| {
                record
                    .object
                    .clone()
                    .map(|object| (object, record.state.clone()))
            })
        } else {
            records.insert(
                key.clone(),
                PadRecord {
                    device_key: key.clone(),
                    source: PadSource::Host,
                    ff_supported: false,
                    exposed: false,
                    saw_neutral: true,
                    index: Some(state.index),
                    state,
                    object: None,
                },
            );
            None
        }
    });
    if let Some((object, state)) = refreshed.flatten() {
        apply_state(&object, &state);
    }
    expose_one(&key);
}

pub fn get_gamepads_value() -> Value {
    Value::function(|_, _| {
        poll();
        if !policy_allowed() {
            w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                "gamepad is disabled by permissions policy",
                "SecurityError",
            ));
        }
        warn_if_unadapted();
        snapshot_list()
    })
}

pub fn reset() {
    RECORDS.with(|records| records.borrow_mut().clear());
    MOCK.with(|slot| *slot.borrow_mut() = None);
    PLAYING.with(|playing| playing.borrow_mut().clear());
    POLICY_ALLOWED.with(|flag| flag.set(true));
    HAS_GESTURE.with(|flag| flag.set(false));
    POLLING.with(|flag| flag.set(false));
    EVENTS.with(|count| count.set(0));
}

pub(crate) fn map_sample(sample: &PlatformSample) -> GamepadState {
    if sample.standard_layout {
        GamepadState {
            id: sample.id.clone(),
            index: 0,
            connected: true,
            timestamp: 0.0,
            mapping: "standard".into(),
            axes: vec![
                standard_axis(sample, AxisRole::LeftX),
                standard_axis(sample, AxisRole::LeftY),
                standard_axis(sample, AxisRole::RightX),
                standard_axis(sample, AxisRole::RightY),
            ],
            buttons: STANDARD_BUTTONS
                .iter()
                .copied()
                .map(|role| standard_button(sample, role))
                .collect(),
        }
    } else {
        GamepadState {
            id: sample.id.clone(),
            index: 0,
            connected: true,
            timestamp: 0.0,
            mapping: String::new(),
            axes: sample
                .axes
                .iter()
                .map(|axis| axis.value.clamp(-1.0, 1.0))
                .collect(),
            buttons: sample
                .buttons
                .iter()
                .map(|button| button_state(button.pressed, button.value))
                .collect(),
        }
    }
}

fn standard_axis(sample: &PlatformSample, role: AxisRole) -> f64 {
    sample
        .axes
        .iter()
        .find(|axis| axis.role == role)
        .map(|axis| axis.value.clamp(-1.0, 1.0))
        .unwrap_or(0.0)
}

fn standard_button(sample: &PlatformSample, role: ButtonRole) -> GamepadButtonState {
    if let Some(button) = sample.buttons.iter().find(|button| button.role == role)
        && (button.pressed || button.value > 0.0)
    {
        return button_state(button.pressed, button.value);
    }
    if dpad_pressed(sample, role) {
        return button_state(true, 1.0);
    }
    button_state(false, 0.0)
}

fn dpad_pressed(sample: &PlatformSample, role: ButtonRole) -> bool {
    let (axis, sign) = match role {
        ButtonRole::DpadUp => (AxisRole::DpadY, -1.0),
        ButtonRole::DpadDown => (AxisRole::DpadY, 1.0),
        ButtonRole::DpadLeft => (AxisRole::DpadX, -1.0),
        ButtonRole::DpadRight => (AxisRole::DpadX, 1.0),
        _ => return false,
    };
    sample
        .axes
        .iter()
        .any(|entry| entry.role == axis && entry.value * sign > 0.5)
}

fn button_state(pressed: bool, value: f64) -> GamepadButtonState {
    let value = value.clamp(0.0, 1.0);
    GamepadButtonState {
        pressed,
        touched: pressed,
        value,
    }
}

fn host_key(index: u32) -> String {
    format!("host:{index}")
}

fn policy_allowed() -> bool {
    POLICY_ALLOWED.with(Cell::get)
}

fn has_gesture() -> bool {
    HAS_GESTURE.with(Cell::get)
}

fn sync_platform() {
    let samples = collect_platform_samples();
    let seen: HashSet<String> = samples
        .iter()
        .map(|sample| sample.device_key.clone())
        .collect();
    for sample in samples {
        upsert_platform(sample);
    }
    let stale = RECORDS.with(|records| {
        records
            .borrow()
            .iter()
            .filter(|&(key, record)| record.source == PadSource::Platform && !seen.contains(key))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>()
    });
    for key in stale {
        disconnect(&key, None);
    }
    expose_pending();
}

fn upsert_platform(sample: PlatformSample) {
    let mut state = map_sample(&sample);
    state.connected = true;
    let key = sample.device_key.clone();
    let refresh = RECORDS.with(|records| {
        let mut records = records.borrow_mut();
        if let Some(record) = records.get_mut(&key) {
            let changed = record.state.buttons != state.buttons
                || record.state.axes != state.axes
                || record.state.mapping != state.mapping
                || record.state.id != state.id;
            record.ff_supported = sample.ff_supported;
            record.state.buttons = state.buttons;
            record.state.axes = state.axes;
            record.state.mapping = state.mapping;
            record.state.id = state.id;
            record.state.connected = true;
            observe_gesture(record);
            if changed && record.exposed {
                record.state.timestamp = crate::jsdom::performance_now().max(0.0);
                record
                    .object
                    .clone()
                    .map(|object| (object, record.state.clone()))
            } else {
                None
            }
        } else {
            let mut record = PadRecord {
                device_key: key.clone(),
                source: PadSource::Platform,
                ff_supported: sample.ff_supported,
                exposed: false,
                saw_neutral: false,
                index: None,
                state,
                object: None,
            };
            observe_gesture(&mut record);
            records.insert(key, record);
            None
        }
    });
    if let Some((object, state)) = refresh {
        apply_state(&object, &state);
    }
}

fn observe_gesture(record: &mut PadRecord) {
    if record.source != PadSource::Platform {
        return;
    }
    if is_neutral(&record.state) {
        record.saw_neutral = true;
        return;
    }
    if record.saw_neutral {
        HAS_GESTURE.with(|flag| flag.set(true));
    }
}

fn is_neutral(state: &GamepadState) -> bool {
    state
        .buttons
        .iter()
        .all(|button| !button.pressed && button.value <= 0.5)
        && state.axes.iter().all(|axis| axis.abs() <= AXIS_GESTURE)
}

fn expose_pending() {
    if !policy_allowed() {
        return;
    }
    let gesture = has_gesture();
    let mut keys = RECORDS.with(|records| {
        records
            .borrow()
            .iter()
            .filter_map(|(key, record)| {
                if record.exposed {
                    return None;
                }
                let ready = record.source == PadSource::Host || gesture;
                ready.then(|| key.clone())
            })
            .collect::<Vec<_>>()
    });
    keys.sort();
    for key in keys {
        expose_one(&key);
    }
}

fn expose_one(key: &str) {
    if !policy_allowed() {
        return;
    }
    let index = RECORDS.with(|records| {
        let records = records.borrow();
        let record = records.get(key)?;
        if record.exposed {
            return None;
        }
        let index = match record.source {
            PadSource::Host => record.state.index,
            PadSource::Platform => lowest_free_index(&records),
        };
        Some(index)
    });
    let Some(index) = index else {
        return;
    };
    let object = RECORDS.with(|records| {
        let mut records = records.borrow_mut();
        let record = records.get_mut(key)?;
        if record.exposed {
            return None;
        }
        record.state.index = index;
        record.index = Some(index);
        if record.source == PadSource::Platform {
            record.state.timestamp = crate::jsdom::performance_now().max(0.0);
        }
        let mode = match record.source {
            PadSource::Host => HapticMode::Compat,
            PadSource::Platform => HapticMode::Platform {
                device_key: record.device_key.clone(),
                ff: record.ff_supported,
            },
        };
        let object = gamepad_value(&record.state, &mode);
        record.object = Some(object.clone());
        record.exposed = true;
        Some(object)
    });
    if let Some(object) = object {
        dispatch_connection_event("gamepadconnected", object);
    }
}

fn lowest_free_index(records: &HashMap<String, PadRecord>) -> u32 {
    let mut index = 0u32;
    while records.values().any(|record| record.index == Some(index)) {
        index = index.saturating_add(1);
        if index == u32::MAX {
            break;
        }
    }
    index
}

fn disconnect(key: &str, timestamp: Option<f64>) {
    let Some(record) = RECORDS.with(|records| records.borrow_mut().remove(key)) else {
        return;
    };
    if record.source == PadSource::Platform {
        preempt_effect(&record.device_key);
        backend_reset(&record.device_key);
    }
    let Some(object) = record.object else {
        return;
    };
    if !record.exposed {
        return;
    }
    let mut state = record.state;
    state.connected = false;
    state.timestamp = timestamp.unwrap_or_else(|| crate::jsdom::performance_now().max(0.0));
    apply_state(&object, &state);
    dispatch_connection_event("gamepaddisconnected", object);
}

fn snapshot_list() -> Value {
    RECORDS.with(|records| {
        let records = records.borrow();
        let mut exposed = records
            .values()
            .filter(|record| record.exposed)
            .filter_map(|record| {
                Some((record.index?, record.object.clone().unwrap_or(Value::Null)))
            })
            .collect::<Vec<_>>();
        if exposed.is_empty() {
            return Value::array(vec![]);
        }
        exposed.sort_by_key(|(index, _)| *index);
        let max = exposed.last().map(|(index, _)| *index).unwrap_or(0);
        let mut values = vec![Value::Null; max as usize + 1];
        for (index, object) in exposed {
            values[index as usize] = object;
        }
        Value::array(values)
    })
}

fn haptic_actuator(mode: &HapticMode) -> Value {
    let supported = match mode {
        HapticMode::Compat => true,
        HapticMode::Platform { ff, .. } => *ff,
    };
    let effects = if supported {
        vec![Value::string("dual-rumble")]
    } else {
        Vec::new()
    };
    let mode_play = mode.clone();
    let mode_reset = mode.clone();
    let actuator = Value::object(HashMap::from([
        ("effects".into(), Value::array(effects)),
        (
            "type".into(),
            Value::string(if supported { "dual-rumble" } else { "" }),
        ),
        (
            "playEffect".into(),
            Value::function(move |_, args| play_effect(&mode_play, &args)),
        ),
        (
            "reset".into(),
            Value::function(move |_, _| reset_effect(&mode_reset)),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &actuator,
        &gamepad_haptic_actuator_class().get_property("prototype"),
    );
    actuator
}

fn play_effect(mode: &HapticMode, args: &[Value]) -> Value {
    let kind = args.first().map(Value::to_js_string).unwrap_or_default();
    let params = args.get(1).cloned().unwrap_or(Value::Undefined);
    let parsed = match parse_effect(&kind, &params) {
        Ok(parsed) => parsed,
        Err(message) => return rejected("TypeError", &message),
    };
    if document_hidden() {
        return rejected("InvalidStateError", "document is hidden");
    }
    match mode {
        HapticMode::Compat => {
            if kind != "dual-rumble" {
                return rejected(
                    "NotSupportedError",
                    "gamepad actuator cannot play this effect",
                );
            }
            warn_compat_haptics();
            w3cos_core::promise::resolve(vec![Value::string("complete")])
        }
        HapticMode::Platform { device_key, ff } => {
            if !pad_connected(device_key) {
                return rejected("InvalidStateError", "gamepad is disconnected");
            }
            preempt_effect(device_key);
            if kind != "dual-rumble" || !*ff {
                warn_unsupported_haptics();
                return rejected(
                    "NotSupportedError",
                    "gamepad actuator cannot play this effect",
                );
            }
            if let Err(message) = backend_play(device_key, &parsed) {
                warn_unsupported_haptics();
                return rejected("NotSupportedError", &message);
            }
            let (promise, resolve, reject) = deferred();
            let deadline = crate::jsdom::performance_now()
                + (parsed.delay_ms.saturating_add(parsed.duration_ms)) as f64;
            PLAYING.with(|playing| {
                playing.borrow_mut().push(Playing {
                    device_key: device_key.clone(),
                    resolve,
                    reject,
                    deadline,
                });
            });
            promise
        }
    }
}

fn reset_effect(mode: &HapticMode) -> Value {
    if document_hidden() {
        return rejected("InvalidStateError", "document is hidden");
    }
    match mode {
        HapticMode::Compat => w3cos_core::promise::resolve(vec![Value::string("complete")]),
        HapticMode::Platform { device_key, .. } => {
            if !pad_connected(device_key) {
                return rejected("InvalidStateError", "gamepad is disconnected");
            }
            preempt_effect(device_key);
            backend_reset(device_key);
            w3cos_core::promise::resolve(vec![Value::string("complete")])
        }
    }
}

fn parse_effect(kind: &str, params: &Value) -> Result<RumbleParams, String> {
    if params.is_null() || (!params.is_undefined() && !params.is_object()) {
        return Err("GamepadEffectParameters must be an object".into());
    }
    let duration = read_millis(params, "duration")?;
    let delay = read_millis(params, "startDelay")?;
    let strong = read_unit(params, "strongMagnitude")?;
    let weak = read_unit(params, "weakMagnitude")?;
    match kind {
        "dual-rumble" => Ok(RumbleParams {
            duration_ms: duration,
            delay_ms: delay,
            strong,
            weak,
        }),
        "trigger-rumble" => {
            let _left = read_unit(params, "leftTrigger")?;
            let _right = read_unit(params, "rightTrigger")?;
            Ok(RumbleParams {
                duration_ms: duration,
                delay_ms: delay,
                strong,
                weak,
            })
        }
        _ => Err("invalid GamepadHapticEffectType".into()),
    }
}

fn read_millis(params: &Value, key: &str) -> Result<u64, String> {
    let number = optional_number(params, key)?.unwrap_or(0.0);
    if number < 0.0 {
        return Err(format!("{key} must be a non-negative finite number"));
    }
    Ok(number.min(MAX_EFFECT_MS) as u64)
}

fn read_unit(params: &Value, key: &str) -> Result<f64, String> {
    let number = optional_number(params, key)?.unwrap_or(0.0);
    if !(0.0..=1.0).contains(&number) {
        return Err(format!("{key} must be in the range 0 to 1"));
    }
    Ok(number)
}

fn optional_number(params: &Value, key: &str) -> Result<Option<f64>, String> {
    if params.is_undefined() || !params.is_object() {
        return Ok(None);
    }
    let value = params.get_property(key);
    if value.is_undefined() {
        return Ok(None);
    }
    if !value.is_number() {
        return Err(format!("{key} must be a finite number"));
    }
    let number = value.to_number();
    if !number.is_finite() {
        return Err(format!("{key} must be a finite number"));
    }
    Ok(Some(number))
}

fn pad_connected(device_key: &str) -> bool {
    RECORDS.with(|records| {
        records
            .borrow()
            .get(device_key)
            .is_some_and(|record| record.exposed && record.state.connected)
    })
}

fn preempt_effect(device_key: &str) {
    let playing = PLAYING.with(|playing| {
        let mut slot = playing.borrow_mut();
        slot.iter()
            .position(|effect| effect.device_key == device_key)
            .map(|index| slot.remove(index))
    });
    if let Some(effect) = playing {
        let _ = effect
            .resolve
            .call(Value::Undefined, vec![Value::string("preempted")]);
    }
}

fn stop_all_playing() {
    let playing = PLAYING.with(|playing| std::mem::take(&mut *playing.borrow_mut()));
    for effect in playing {
        backend_reset(&effect.device_key);
        let _ = effect
            .resolve
            .call(Value::Undefined, vec![Value::string("preempted")]);
    }
}

fn settle_haptics() {
    let failed = backend_failures();
    let playing = PLAYING.with(|playing| std::mem::take(&mut *playing.borrow_mut()));
    if playing.is_empty() && failed.is_empty() {
        return;
    }
    let now = crate::jsdom::performance_now();
    let mut keep = Vec::new();
    for effect in playing {
        if failed.iter().any(|key| key == &effect.device_key) {
            let _ = effect.reject.call(
                Value::Undefined,
                vec![w3cos_core::web::dom_exception_instance(
                    "gamepad actuator failed to play the effect",
                    "NotSupportedError",
                )],
            );
        } else if now >= effect.deadline {
            let _ = effect
                .resolve
                .call(Value::Undefined, vec![Value::string("complete")]);
        } else {
            keep.push(effect);
        }
    }
    PLAYING.with(|playing| {
        let mut slot = playing.borrow_mut();
        keep.append(&mut slot);
        *slot = keep;
    });
}

fn deferred() -> (Value, Value, Value) {
    let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
    let reject_slot = Rc::new(RefCell::new(Value::Undefined));
    let resolve_for_executor = Rc::clone(&resolve_slot);
    let reject_for_executor = Rc::clone(&reject_slot);
    let promise = w3cos_core::promise::new(vec![Value::function(move |_, args| {
        *resolve_for_executor.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
        *reject_for_executor.borrow_mut() = args.get(1).cloned().unwrap_or(Value::Undefined);
        Value::Undefined
    })]);
    (
        promise,
        resolve_slot.borrow().clone(),
        reject_slot.borrow().clone(),
    )
}

fn rejected(name: &str, message: &str) -> Value {
    let error = if name == "TypeError" {
        Value::object(HashMap::from([
            ("name".into(), Value::string(name)),
            ("message".into(), Value::string(message)),
        ]))
    } else {
        w3cos_core::web::dom_exception_instance(message, name)
    };
    w3cos_core::promise::reject(vec![error])
}

fn document_hidden() -> bool {
    crate::jsdom::document_value()
        .get_property("visibilityState")
        .to_js_string()
        == "hidden"
}

fn button_value(state: GamepadButtonState) -> Value {
    let button = Value::object(HashMap::from([
        ("pressed".into(), Value::Bool(state.pressed)),
        ("touched".into(), Value::Bool(state.touched)),
        ("value".into(), Value::Number(state.value.clamp(0.0, 1.0))),
    ]));
    w3cos_core::class::set_prototype_of(&button, &gamepad_button_class().get_property("prototype"));
    button
}

fn apply_state(gamepad: &Value, state: &GamepadState) {
    gamepad.set_property("id", Value::string(&state.id));
    gamepad.set_property("index", Value::Number(state.index as f64));
    gamepad.set_property("connected", Value::Bool(state.connected));
    gamepad.set_property("timestamp", Value::Number(state.timestamp.max(0.0)));
    gamepad.set_property("mapping", Value::string(&state.mapping));
    gamepad.set_property(
        "axes",
        Value::array(
            state
                .axes
                .iter()
                .map(|axis| Value::Number(axis.clamp(-1.0, 1.0)))
                .collect(),
        ),
    );
    gamepad.set_property(
        "buttons",
        Value::array(state.buttons.iter().copied().map(button_value).collect()),
    );
}

fn gamepad_value(state: &GamepadState, mode: &HapticMode) -> Value {
    let gamepad = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(&gamepad, &gamepad_class().get_property("prototype"));
    gamepad.set_property("vibrationActuator", haptic_actuator(mode));
    apply_state(&gamepad, state);
    gamepad
}

fn dispatch_connection_event(event_type: &str, gamepad: Value) {
    if !policy_allowed() {
        return;
    }
    let event = w3cos_core::class::construct(
        &gamepad_event_class(),
        vec![
            Value::string(event_type),
            Value::object(HashMap::from([("gamepad".into(), gamepad)])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    let window = crate::jsdom::window_value();
    window.call_method("dispatchEvent", vec![event.clone()]);
    let handler = window.get_property(&format!("on{event_type}"));
    if handler.is_function() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler.call(window, vec![event]);
        }));
    }
    EVENTS.with(|count| count.set(count.get() + 1));
}

fn warn_if_unadapted() {
    match platform_backend_name() {
        "gilrs" | "mock" => {}
        "gilrs-unavailable" => {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: gilrs gamepad adapter failed to start: {}",
                    adapter_error()
                );
            });
        }
        _ => {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: navigator.getGamepads exposes host-injectable controller \
                     snapshots; detecting physical controllers requires a platform gamepad adapter"
                );
            });
        }
    }
}

fn warn_compat_haptics() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: GamepadHapticActuator returns compatible completion \
             results; physical vibration requires a platform gamepad haptics adapter"
        );
    });
}

fn warn_unsupported_haptics() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: GamepadHapticActuator cannot play this effect on the \
             connected controller"
        );
    });
}

fn collect_platform_samples() -> Vec<PlatformSample> {
    if let Some(pads) = MOCK.with(|slot| slot.borrow().as_ref().map(|backend| backend.pads.clone()))
    {
        return pads;
    }
    desktop_samples()
}

fn backend_play(device_key: &str, params: &RumbleParams) -> Result<(), String> {
    let mocked = MOCK.with(|slot| {
        slot.borrow_mut().as_mut().map(|backend| {
            if backend.fail_play {
                return Err("mock gamepad rejected force feedback".to_string());
            }
            backend.plays.push(RecordedRumble {
                device_key: device_key.to_string(),
                strong: params.strong,
                weak: params.weak,
                delay_ms: params.delay_ms,
                duration_ms: params.duration_ms,
            });
            Ok(())
        })
    });
    if let Some(result) = mocked {
        return result;
    }
    desktop_play(device_key, params)
}

fn backend_reset(device_key: &str) {
    let mocked = MOCK.with(|slot| {
        if let Some(backend) = slot.borrow_mut().as_mut() {
            backend.resets.push(device_key.to_string());
            true
        } else {
            false
        }
    });
    if !mocked {
        desktop_reset(device_key);
    }
}

fn backend_failures() -> Vec<String> {
    if MOCK.with(|slot| slot.borrow().is_some()) {
        return Vec::new();
    }
    desktop_failures()
}

#[cfg(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
))]
fn desktop_samples() -> Vec<PlatformSample> {
    crate::gamepad_gilrs::samples()
}

#[cfg(not(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
)))]
fn desktop_samples() -> Vec<PlatformSample> {
    Vec::new()
}

#[cfg(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
))]
fn desktop_backend_name() -> &'static str {
    crate::gamepad_gilrs::backend_name()
}

#[cfg(not(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
)))]
fn desktop_backend_name() -> &'static str {
    "host"
}

#[cfg(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
))]
fn desktop_error() -> String {
    crate::gamepad_gilrs::last_error().unwrap_or_default()
}

#[cfg(not(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
)))]
fn desktop_error() -> String {
    String::new()
}

#[cfg(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
))]
fn desktop_play(device_key: &str, params: &RumbleParams) -> Result<(), String> {
    crate::gamepad_gilrs::play(
        device_key,
        params.strong as f32,
        params.weak as f32,
        params.delay_ms.min(u64::from(u32::MAX)) as u32,
        params.duration_ms.min(u64::from(u32::MAX)) as u32,
    )
}

#[cfg(not(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
)))]
fn desktop_play(_device_key: &str, _params: &RumbleParams) -> Result<(), String> {
    Err("no gamepad haptics adapter is available".into())
}

#[cfg(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
))]
fn desktop_reset(device_key: &str) {
    crate::gamepad_gilrs::reset(device_key);
}

#[cfg(not(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
)))]
fn desktop_reset(_device_key: &str) {}

#[cfg(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
))]
fn desktop_failures() -> Vec<String> {
    crate::gamepad_gilrs::take_haptic_failures()
}

#[cfg(not(all(
    feature = "web-gamepad",
    any(target_os = "macos", target_os = "windows", target_os = "linux"),
    not(target_env = "ohos")
)))]
fn desktop_failures() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn controller(connected: bool) -> GamepadState {
        GamepadState {
            id: "W3COS Controller".into(),
            index: 1,
            connected,
            timestamp: 42.0,
            mapping: "standard".into(),
            axes: vec![-0.5, 0.25],
            buttons: vec![GamepadButtonState {
                pressed: true,
                touched: true,
                value: 0.75,
            }],
        }
    }

    fn standard_sample(pressed: bool, axis_y: f64, ff: bool) -> PlatformSample {
        let mut buttons = STANDARD_BUTTONS
            .iter()
            .copied()
            .map(|role| SampleButton {
                role,
                value: 0.0,
                pressed: false,
            })
            .collect::<Vec<_>>();
        buttons[0] = SampleButton {
            role: ButtonRole::South,
            value: if pressed { 1.0 } else { 0.0 },
            pressed,
        };
        PlatformSample {
            device_key: "pad-a".into(),
            id: "Test Pad".into(),
            standard_layout: true,
            ff_supported: ff,
            buttons,
            axes: vec![
                SampleAxis {
                    role: AxisRole::LeftX,
                    value: 0.0,
                },
                SampleAxis {
                    role: AxisRole::LeftY,
                    value: axis_y,
                },
                SampleAxis {
                    role: AxisRole::RightX,
                    value: 0.0,
                },
                SampleAxis {
                    role: AxisRole::RightY,
                    value: 0.0,
                },
            ],
        }
    }

    fn install(sample: PlatformSample) {
        set_mock_gamepad_backend(Some(MockGamepadBackend {
            pads: vec![sample],
            ..MockGamepadBackend::default()
        }));
    }

    fn replace_pad(sample: PlatformSample) {
        mock_gamepad_backend_mut(|backend| backend.pads = vec![sample]);
    }

    fn gamepads() -> Value {
        get_gamepads_value().call(Value::Undefined, vec![])
    }

    fn listen(log: &Rc<RefCell<Vec<String>>>) {
        for event_type in ["gamepadconnected", "gamepaddisconnected"] {
            let log = Rc::clone(log);
            crate::jsdom::window_value().call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, args| {
                        let event = &args[0];
                        log.borrow_mut().push(format!(
                            "{}:{}:{}",
                            event.get_property("type").to_js_string(),
                            event
                                .get_property("gamepad")
                                .get_property("index")
                                .to_js_string(),
                            event.get_property("isTrusted").to_bool()
                        ));
                        Value::Undefined
                    }),
                ],
            );
        }
    }

    fn watch(promise: Value) -> Rc<RefCell<Option<Result<String, String>>>> {
        let slot = Rc::new(RefCell::new(None));
        let ok = Rc::clone(&slot);
        let err = Rc::clone(&slot);
        promise.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *ok.borrow_mut() = Some(Ok(args
                    .first()
                    .map(Value::to_js_string)
                    .unwrap_or_default()));
                Value::Undefined
            })],
        );
        promise.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                let name = args
                    .first()
                    .map(|value| value.get_property("name").to_js_string())
                    .unwrap_or_default();
                *err.borrow_mut() = Some(Err(name));
                Value::Undefined
            })],
        );
        slot
    }

    fn settle(promise: Value) -> Result<String, String> {
        let slot = watch(promise);
        crate::jsdom::drain_microtasks();
        slot.borrow().clone().unwrap_or(Err("unsettled".into()))
    }

    #[test]
    fn host_updates_stable_index_snapshot_and_connection_events() {
        reset();
        let event_log = Rc::new(RefCell::new(Vec::<String>::new()));
        for event_type in ["gamepadconnected", "gamepaddisconnected"] {
            let event_log_for_listener = Rc::clone(&event_log);
            crate::jsdom::window_value().call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, args| {
                        event_log_for_listener.borrow_mut().push(format!(
                            "{}:{}",
                            args[0].get_property("type").to_js_string(),
                            args[0]
                                .get_property("gamepad")
                                .get_property("index")
                                .to_js_string()
                        ));
                        Value::Undefined
                    }),
                ],
            );
        }

        update_gamepad(controller(true));
        let listed = gamepads();
        assert_eq!(listed.get_property("length").to_u32(), 2);
        assert!(listed.get_property("0").is_null());
        let gamepad = listed.get_property("1");
        assert!(w3cos_core::class::instance_of(&gamepad, &gamepad_class()));
        assert!(w3cos_core::class::instance_of(
            &gamepad.get_property("buttons").get_property("0"),
            &gamepad_button_class()
        ));
        assert_eq!(
            gamepad.get_property("axes").get_property("0").to_number(),
            -0.5
        );
        assert_eq!(
            gamepad
                .get_property("buttons")
                .get_property("0")
                .get_property("value")
                .to_number(),
            0.75
        );
        let actuator = gamepad.get_property("vibrationActuator");
        assert!(w3cos_core::class::instance_of(
            &actuator,
            &gamepad_haptic_actuator_class()
        ));
        assert_eq!(
            actuator
                .get_property("effects")
                .get_property("0")
                .to_js_string(),
            "dual-rumble"
        );
        let haptic_result = Rc::new(RefCell::new(String::new()));
        let result_for_callback = Rc::clone(&haptic_result);
        actuator
            .call_method(
                "playEffect",
                vec![Value::string("dual-rumble"), Value::object(HashMap::new())],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *result_for_callback.borrow_mut() = args[0].to_js_string();
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*haptic_result.borrow(), "complete");

        update_gamepad(controller(false));
        assert_eq!(gamepads().get_property("length").to_u32(), 0);
        assert_eq!(
            &*event_log.borrow(),
            &["gamepadconnected:1", "gamepaddisconnected:1"]
        );
    }

    #[test]
    fn standard_mapping_is_seventeen_buttons_and_four_axes() {
        let mut sample = standard_sample(true, 0.25, true);
        sample.axes[2].value = -2.0;
        let state = map_sample(&sample);
        assert_eq!(state.mapping, "standard");
        assert_eq!(state.buttons.len(), 17);
        assert_eq!(state.axes.len(), 4);
        assert!(state.buttons[0].pressed);
        assert!(state.buttons[0].touched);
        assert_eq!(state.buttons[0].value, 1.0);
        assert!(!state.buttons[1].pressed);
        assert_eq!(state.axes[1], 0.25);
        assert_eq!(state.axes[2], -1.0);
    }

    #[test]
    fn unrecognized_layout_keeps_raw_order_and_empty_mapping() {
        let sample = PlatformSample {
            device_key: "raw".into(),
            id: "Unknown".into(),
            standard_layout: false,
            ff_supported: false,
            buttons: vec![
                SampleButton {
                    role: ButtonRole::Other,
                    value: 0.2,
                    pressed: false,
                },
                SampleButton {
                    role: ButtonRole::South,
                    value: 1.0,
                    pressed: true,
                },
            ],
            axes: vec![SampleAxis {
                role: AxisRole::Other,
                value: -0.5,
            }],
        };
        let state = map_sample(&sample);
        assert_eq!(state.mapping, "");
        assert_eq!(state.buttons.len(), 2);
        assert_eq!(state.axes, vec![-0.5]);
        assert!(!state.buttons[0].pressed);
        assert!(state.buttons[1].pressed);
    }

    #[test]
    fn dpad_axis_fills_standard_buttons() {
        let mut sample = standard_sample(false, 0.0, false);
        sample.axes.push(SampleAxis {
            role: AxisRole::DpadY,
            value: -1.0,
        });
        sample.axes.push(SampleAxis {
            role: AxisRole::DpadX,
            value: 1.0,
        });
        let state = map_sample(&sample);
        assert!(state.buttons[12].pressed);
        assert_eq!(state.buttons[12].value, 1.0);
        assert!(!state.buttons[13].pressed);
        assert!(state.buttons[15].pressed);
        assert!(!state.buttons[14].pressed);
        assert_eq!(state.axes.len(), 4);
    }

    #[test]
    fn mock_backend_waits_for_a_gesture_and_keeps_snapshots() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        reset();
        let log = Rc::new(RefCell::new(Vec::new()));
        listen(&log);
        install(standard_sample(true, 0.0, true));
        assert_eq!(gamepads().get_property("length").to_u32(), 0);
        assert!(log.borrow().is_empty());

        replace_pad(standard_sample(false, 0.0, true));
        assert_eq!(gamepads().get_property("length").to_u32(), 0);

        replace_pad(standard_sample(true, 0.25, true));
        let listed = gamepads();
        assert_eq!(listed.get_property("length").to_u32(), 1);
        let pad = listed.get_property("0");
        assert_eq!(pad.get_property("mapping").to_js_string(), "standard");
        assert_eq!(
            pad.get_property("buttons").get_property("length").to_u32(),
            17
        );
        assert_eq!(pad.get_property("axes").get_property("length").to_u32(), 4);
        assert_eq!(
            pad.get_property("buttons")
                .get_property("0")
                .get_property("value")
                .to_number(),
            1.0
        );
        assert_eq!(&*log.borrow(), &["gamepadconnected:0:true".to_string()]);
        pad.set_property("marker", Value::string("held"));
        let timestamp = pad.get_property("timestamp").to_number();

        replace_pad(standard_sample(false, 0.25, true));
        assert_eq!(
            pad.get_property("buttons")
                .get_property("0")
                .get_property("value")
                .to_number(),
            1.0
        );
        let again = gamepads();
        let same = again.get_property("0");
        assert_eq!(same.get_property("marker").to_js_string(), "held");
        assert_eq!(
            same.get_property("buttons")
                .get_property("0")
                .get_property("value")
                .to_number(),
            0.0
        );
        assert!(same.get_property("timestamp").to_number() >= timestamp);

        replace_pad(standard_sample(false, 0.25, true));
        set_mock_gamepad_backend(Some(MockGamepadBackend::default()));
        assert_eq!(gamepads().get_property("length").to_u32(), 0);
        assert_eq!(
            log.borrow().get(1).map(String::as_str),
            Some("gamepaddisconnected:0:true")
        );
        reset();
    }

    #[test]
    fn mock_haptics_complete_preempt_and_reject() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        reset();
        install(standard_sample(false, 0.0, true));
        let _ = gamepads();
        replace_pad(standard_sample(true, 0.0, true));
        let pad = gamepads().get_property("0");
        let actuator = pad.get_property("vibrationActuator");
        assert_eq!(
            actuator
                .get_property("effects")
                .get_property("0")
                .to_js_string(),
            "dual-rumble"
        );

        let first = actuator.call_method(
            "playEffect",
            vec![
                Value::string("dual-rumble"),
                Value::object(HashMap::from([("duration".into(), Value::Number(5_000.0))])),
            ],
        );
        let first_result = watch(first);
        let second = actuator.call_method(
            "playEffect",
            vec![
                Value::string("dual-rumble"),
                Value::object(HashMap::from([
                    ("duration".into(), Value::Number(0.0)),
                    ("strongMagnitude".into(), Value::Number(0.5)),
                    ("weakMagnitude".into(), Value::Number(0.25)),
                ])),
            ],
        );
        assert_eq!(settle(second).unwrap(), "complete");
        assert_eq!(first_result.borrow().clone().unwrap().unwrap(), "preempted");
        let plays = mock_gamepad_backend_mut(|backend| backend.plays.clone()).unwrap();
        assert_eq!(plays.len(), 2);
        assert_eq!(plays[1].strong, 0.5);
        assert_eq!(plays[1].weak, 0.25);

        let held = actuator.call_method(
            "playEffect",
            vec![
                Value::string("dual-rumble"),
                Value::object(HashMap::from([("duration".into(), Value::Number(5_000.0))])),
            ],
        );
        let held_result = watch(held);
        let reset_result = settle(actuator.call_method("reset", vec![]));
        assert_eq!(reset_result.unwrap(), "complete");
        crate::jsdom::drain_microtasks();
        assert_eq!(held_result.borrow().clone().unwrap().unwrap(), "preempted");
        assert!(
            mock_gamepad_backend_mut(|backend| backend.resets.clone())
                .unwrap()
                .contains(&"pad-a".to_string())
        );

        let invalid = settle(actuator.call_method(
            "playEffect",
            vec![
                Value::string("dual-rumble"),
                Value::object(HashMap::from([(
                    "strongMagnitude".into(),
                    Value::Number(2.0),
                )])),
            ],
        ));
        assert_eq!(invalid.unwrap_err(), "TypeError");

        let trigger = settle(actuator.call_method(
            "playEffect",
            vec![
                Value::string("trigger-rumble"),
                Value::object(HashMap::new()),
            ],
        ));
        assert_eq!(trigger.unwrap_err(), "NotSupportedError");

        let mut quiet_pad = standard_sample(false, 0.0, false);
        quiet_pad.device_key = "pad-quiet".into();
        quiet_pad.id = "Quiet Pad".into();
        mock_gamepad_backend_mut(|backend| backend.pads.push(quiet_pad));
        let quiet = gamepads()
            .get_property("1")
            .get_property("vibrationActuator");
        assert_eq!(
            quiet
                .get_property("effects")
                .get_property("length")
                .to_u32(),
            0
        );
        let unsupported = settle(quiet.call_method(
            "playEffect",
            vec![Value::string("dual-rumble"), Value::object(HashMap::new())],
        ));
        assert_eq!(unsupported.unwrap_err(), "NotSupportedError");

        crate::jsdom::set_document_visibility("hidden");
        let hidden = settle(actuator.call_method(
            "playEffect",
            vec![Value::string("dual-rumble"), Value::object(HashMap::new())],
        ));
        assert_eq!(hidden.unwrap_err(), "InvalidStateError");
        crate::jsdom::set_document_visibility("visible");
        reset();
    }

    #[test]
    fn permissions_policy_blocks_enumeration_and_events() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        reset();
        let log = Rc::new(RefCell::new(Vec::new()));
        listen(&log);
        set_gamepad_permissions_policy(false);
        install(standard_sample(false, 0.0, true));
        assert!(gamepads_throw().contains("SecurityError"));
        replace_pad(standard_sample(true, 0.0, true));
        assert!(gamepads_throw().contains("SecurityError"));
        assert!(log.borrow().is_empty());
        set_gamepad_permissions_policy(true);
        assert_eq!(
            log.borrow().as_slice(),
            ["gamepadconnected:0:true".to_string()]
        );
        assert_eq!(gamepads().get_property("length").to_u32(), 1);

        set_gamepad_permissions_policy(false);
        update_gamepad(controller(true));
        assert!(log.borrow().len() == 1);
        assert!(gamepads_throw().contains("SecurityError"));
        set_gamepad_permissions_policy(true);
        assert!(
            log.borrow()
                .iter()
                .any(|entry| entry.starts_with("gamepadconnected:1"))
        );
        reset();
    }

    fn gamepads_throw() -> String {
        let thrown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            get_gamepads_value().call(Value::Undefined, vec![])
        }));
        match thrown {
            Ok(_) => "returned".into(),
            Err(payload) => payload
                .downcast_ref::<w3cos_core::PanicValue>()
                .map(|value| value.0.get_property("name").to_js_string())
                .unwrap_or_else(|| "panic".into()),
        }
    }

    #[test]
    fn hardware_samples_follow_w3c_mapping_when_requested() {
        if std::env::var_os("W3COS_GAMEPAD_DEVICE_TESTS").is_none() {
            return;
        }
        let samples = collect_platform_samples();
        assert_eq!(platform_backend_name(), "gilrs", "{}", adapter_error());
        for sample in samples {
            let state = map_sample(&sample);
            if sample.standard_layout {
                assert_eq!(state.mapping, "standard");
                assert_eq!(state.buttons.len(), 17);
                assert_eq!(state.axes.len(), 4);
            } else {
                assert_eq!(state.mapping, "");
                assert_eq!(state.buttons.len(), sample.buttons.len());
                assert_eq!(state.axes.len(), sample.axes.len());
            }
        }
    }
}
