//! Web Audio graph, buffer and lifecycle layer.
//!
//! Connected graphs render through [`crate::audio_render`]: oscillator,
//! buffer-source, constant-source, gain, stereo-panner and
//! `MediaStreamAudioSourceNode` (live microphone PCM) produce real samples in
//! `OfflineAudioContext.startRendering()` and, after `AudioContext.resume()`,
//! on the default output device via [`crate::audio_device`] (cpal). Other
//! processing nodes render silence with a warning. Compressed decoding,
//! AudioParam automation timelines, sink selection and AudioWorklet processor
//! execution remain pending.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Once};

use crate::audio_render::{GraphSnapshot, NodeKind, NodeSnapshot, Schedule, Waveform};

use w3cos_core::Value;

use crate::jsdom::realm_function;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static CONTEXTS: RefCell<Vec<(Value, Rc<RefCell<ContextClock>>)>> =
        const { RefCell::new(Vec::new()) };
    static CALLBACK_TARGETS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static NEXT_AUDIO_ID: Cell<u64> = const { Cell::new(1) };
    /// Nodes with at least one outgoing connection, kept alive like a
    /// browser keeps connected nodes alive, keyed by node id.
    static GRAPH_NODES: RefCell<HashMap<u64, GraphEntry>> = RefCell::new(HashMap::new());
    /// AudioBufferSourceNode content acquired at `start()`.
    static ACQUIRED_BUFFERS: RefCell<HashMap<u64, (Arc<Vec<Vec<f32>>>, f64)>> =
        RefCell::new(HashMap::new());
    static LIVE_OUTPUTS: RefCell<HashMap<u64, LiveContext>> = RefCell::new(HashMap::new());
    static LAST_LIVE_POLL: Cell<f64> = const { Cell::new(f64::NEG_INFINITY) };
}

struct GraphEntry {
    node: Value,
    context_id: u64,
    kind: &'static str,
    connections: Rc<RefCell<Vec<Value>>>,
}

struct LiveContext {
    output: crate::audio_device::LiveOutput,
    destination: u64,
}

fn next_audio_id() -> u64 {
    NEXT_AUDIO_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    })
}

fn audio_id(value: &Value) -> Option<u64> {
    let id = value.get_property("__w3cos_audio_id");
    id.is_number().then(|| id.to_number() as u64)
}

fn param_number(node: &Value, name: &str, fallback: f64) -> f64 {
    let param = node.get_property(name);
    if param.is_undefined() || param.is_null() {
        return fallback;
    }
    let value = param.get_property("value").to_number();
    if value.is_finite() { value } else { fallback }
}

fn optional_time(node: &Value, name: &str) -> Option<f64> {
    let value = node.get_property(name);
    value.is_number().then(|| value.to_number())
}

fn schedule_of(node: &Value) -> Schedule {
    Schedule {
        start: optional_time(node, "__w3cos_start_time"),
        stop: optional_time(node, "__w3cos_stop_time"),
    }
}

fn numbers(value: &Value) -> Vec<f64> {
    if value.is_undefined() || value.is_null() {
        return Vec::new();
    }
    value.iter().map(|item| item.to_number()).collect()
}

/// Copies an AudioBuffer's channel data out of its Float32Arrays.
fn buffer_channels(buffer: &Value) -> Option<(Arc<Vec<Vec<f32>>>, f64)> {
    if !w3cos_core::class::instance_of(buffer, &class_for("AudioBuffer")) {
        return None;
    }
    let count = buffer.get_property("numberOfChannels").to_u32() as usize;
    let rate = buffer.get_property("sampleRate").to_number();
    let channels = (0..count)
        .map(|channel| {
            let data = buffer.call_method("getChannelData", vec![Value::Number(channel as f64)]);
            w3cos_core::binary::bytes_of(&data)
                .unwrap_or_default()
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .collect()
        })
        .collect();
    Some((Arc::new(channels), rate))
}

fn acquire_buffer(node: &Value, id: u64) {
    if let Some(content) = buffer_channels(&node.get_property("buffer")) {
        ACQUIRED_BUFFERS.with(|buffers| buffers.borrow_mut().insert(id, content));
    }
}

fn node_kind(id: u64, node: &Value, kind: &'static str, unsupported: &mut Vec<String>) -> NodeKind {
    match kind {
        "GainNode" => NodeKind::Gain {
            gain: param_number(node, "gain", 1.0) as f32,
        },
        "StereoPannerNode" => NodeKind::StereoPanner {
            pan: param_number(node, "pan", 0.0) as f32,
        },
        "ConstantSourceNode" => NodeKind::ConstantSource {
            offset: param_number(node, "offset", 1.0) as f32,
            schedule: schedule_of(node),
        },
        "OscillatorNode" => {
            let waveform = match node.get_property("type").to_js_string().as_str() {
                "square" => Some(Waveform::Square),
                "sawtooth" => Some(Waveform::Sawtooth),
                "triangle" => Some(Waveform::Triangle),
                "custom" => None,
                _ => Some(Waveform::Sine),
            };
            let table = waveform.is_none().then(|| {
                let wave = node.get_property("__w3cos_periodic_wave");
                Arc::new(crate::audio_render::periodic_wave_table(
                    &numbers(&wave.get_property("__w3cos_real")),
                    &numbers(&wave.get_property("__w3cos_imag")),
                    !wave.get_property("__w3cos_disable_normalization").to_bool(),
                ))
            });
            NodeKind::Oscillator {
                waveform,
                table,
                frequency: param_number(node, "frequency", 440.0),
                detune: param_number(node, "detune", 0.0),
                schedule: schedule_of(node),
            }
        }
        "AudioBufferSourceNode" => {
            if ACQUIRED_BUFFERS.with(|buffers| !buffers.borrow().contains_key(&id)) {
                acquire_buffer(node, id);
            }
            let Some((channels, buffer_rate)) =
                ACQUIRED_BUFFERS.with(|buffers| buffers.borrow().get(&id).cloned())
            else {
                return NodeKind::Silent;
            };
            NodeKind::BufferSource {
                channels,
                buffer_rate,
                playback_rate: param_number(node, "playbackRate", 1.0),
                detune: param_number(node, "detune", 0.0),
                looping: node.get_property("loop").to_bool(),
                loop_start: node.get_property("loopStart").to_number().max(0.0),
                loop_end: node.get_property("loopEnd").to_number().max(0.0),
                offset: optional_time(node, "__w3cos_start_offset").unwrap_or(0.0),
                duration: optional_time(node, "__w3cos_start_duration"),
                schedule: schedule_of(node),
            }
        }
        "MediaStreamAudioSourceNode" => {
            if let Some(source) = media_stream_source(node) {
                return NodeKind::MediaStream {
                    queue: source.queue,
                    audibility: source.audibility,
                    channels: source.channels,
                    sample_rate: source.sample_rate,
                };
            }
            if !unsupported
                .iter()
                .any(|name| name == "MediaStreamAudioSourceNode")
            {
                unsupported.push("MediaStreamAudioSourceNode".to_string());
            }
            NodeKind::Silent
        }
        other => {
            if !unsupported.iter().any(|name| name == other) {
                unsupported.push(other.to_string());
            }
            NodeKind::Silent
        }
    }
}

/// Builds a renderable snapshot of every connected node in a context.
fn graph_snapshot(context_id: u64, destination: u64) -> GraphSnapshot {
    let entries: Vec<(u64, Value, &'static str, Vec<Value>)> = GRAPH_NODES.with(|nodes| {
        nodes
            .borrow()
            .iter()
            .filter(|(_, entry)| entry.context_id == context_id)
            .map(|(id, entry)| {
                (
                    *id,
                    entry.node.clone(),
                    entry.kind,
                    entry.connections.borrow().clone(),
                )
            })
            .collect()
    });
    let mut snapshot = GraphSnapshot {
        destination,
        ..GraphSnapshot::empty()
    };
    snapshot.nodes.insert(
        destination,
        NodeSnapshot {
            kind: NodeKind::Destination,
            inputs: Vec::new(),
        },
    );
    for (id, node, kind, _) in &entries {
        let kind = node_kind(*id, node, kind, &mut snapshot.unsupported);
        snapshot.nodes.insert(
            *id,
            NodeSnapshot {
                kind,
                inputs: Vec::new(),
            },
        );
    }
    for (id, _, _, connections) in &entries {
        for target in connections {
            match audio_id(target) {
                Some(target_id) => {
                    if let Some(node) = snapshot.nodes.get_mut(&target_id) {
                        node.inputs.push(*id);
                    }
                }
                None => {
                    let name = "AudioParam input".to_string();
                    if !snapshot.unsupported.contains(&name) {
                        snapshot.unsupported.push(name);
                    }
                }
            }
        }
    }
    for node in snapshot.nodes.values_mut() {
        node.inputs.sort_unstable();
    }
    if !snapshot.unsupported.is_empty() {
        static WARNING: Once = Once::new();
        let names = snapshot.unsupported.join(", ");
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: Web Audio renders oscillator, buffer-source, constant-source, \
                 gain, stereo-panner and media-stream source nodes; {names} render silence until \
                 their DSP lands"
            );
        });
    }
    snapshot
}

/// The first audio track of a `MediaStreamAudioSourceNode`, when that track
/// is backed by a live capture queue.
fn media_stream_source(node: &Value) -> Option<crate::audio_input::RenderSource> {
    let stream = node.get_property("mediaStream");
    if !w3cos_core::class::instance_of(&stream, &crate::media_devices_web::media_stream_class()) {
        return None;
    }
    let track = stream
        .call_method("getAudioTracks", vec![])
        .get_property("0");
    if track.is_undefined() || track.is_null() {
        return None;
    }
    crate::audio_input::source_for_track(&track.get_property("id").to_js_string())
}

fn context_ids(node: &Value) -> Option<(u64, u64)> {
    let context = node.get_property("context");
    let context_id = audio_id(&context)?;
    let destination = context.get_property("__w3cos_audio_destination_id");
    destination
        .is_number()
        .then(|| (context_id, destination.to_number() as u64))
}

/// Publishes the current graph to a running device output, if any.
fn refresh_live(context_id: u64) {
    let destination = LIVE_OUTPUTS.with(|outputs| {
        outputs
            .borrow()
            .get(&context_id)
            .map(|live| live.destination)
    });
    if let Some(destination) = destination {
        let snapshot = graph_snapshot(context_id, destination);
        LIVE_OUTPUTS.with(|outputs| {
            if let Some(live) = outputs.borrow().get(&context_id) {
                live.output.update(snapshot);
            }
        });
    }
}

fn refresh_node_context(node: &Value) {
    if let Some((context_id, _)) = context_ids(node) {
        refresh_live(context_id);
    }
}

/// Whether `AudioContext.resume()` may open the host output device. Unit
/// tests never open a device unless `W3COS_AUDIO_DEVICE_TESTS` is set, and
/// `W3COS_AUDIO_OUTPUT=0` disables device output at runtime.
fn device_output_enabled() -> bool {
    if std::env::var_os("W3COS_AUDIO_OUTPUT").is_some_and(|value| value == "0") {
        return false;
    }
    !cfg!(test) || std::env::var_os("W3COS_AUDIO_DEVICE_TESTS").is_some()
}

fn open_live_output(context: &Value, sample_rate: f64, start_time: f64) -> bool {
    let (Some(context_id), destination) = (
        audio_id(context),
        context.get_property("__w3cos_audio_destination_id"),
    ) else {
        return false;
    };
    let destination = destination.to_number() as u64;
    let exists = LIVE_OUTPUTS.with(|outputs| {
        let outputs = outputs.borrow();
        if let Some(live) = outputs.get(&context_id) {
            live.output.set_paused(false);
            true
        } else {
            false
        }
    });
    if exists {
        refresh_live(context_id);
        return true;
    }
    if !device_output_enabled() {
        return false;
    }
    match crate::audio_device::LiveOutput::open(sample_rate, start_time) {
        Ok(output) => {
            LIVE_OUTPUTS.with(|outputs| {
                outputs.borrow_mut().insert(
                    context_id,
                    LiveContext {
                        output,
                        destination,
                    },
                )
            });
            refresh_live(context_id);
            true
        }
        Err(error) => {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: AudioContext has no audio output device ({error}); \
                     the graph clock runs silently"
                );
            });
            false
        }
    }
}

/// Republishes live graphs (AudioParam values are sampled, not observed) and
/// delivers `ended` for buffer sources that played to their natural end.
/// Called from the runtime's event pump; returns the number of events run.
pub fn poll_live_outputs() -> usize {
    if LIVE_OUTPUTS.with(|outputs| outputs.borrow().is_empty()) {
        return 0;
    }
    let now = crate::jsdom::performance_now();
    if now - LAST_LIVE_POLL.with(Cell::get) < 20.0 {
        return 0;
    }
    LAST_LIVE_POLL.with(|last| last.set(now));
    let contexts: Vec<u64> =
        LIVE_OUTPUTS.with(|outputs| outputs.borrow().keys().copied().collect());
    let mut ran = 0;
    for context_id in contexts {
        refresh_live(context_id);
        let finished = LIVE_OUTPUTS.with(|outputs| {
            outputs
                .borrow()
                .get(&context_id)
                .map(|live| live.output.take_finished())
                .unwrap_or_default()
        });
        for id in finished {
            let node =
                GRAPH_NODES.with(|nodes| nodes.borrow().get(&id).map(|entry| entry.node.clone()));
            if let Some(node) = node {
                if !node.get_property("__w3cos_ended").to_bool() {
                    node.set_property("__w3cos_ended", Value::Bool(true));
                    dispatch(&node, "ended");
                    ran += 1;
                }
            }
        }
    }
    ran
}

/// Number of frames the device has rendered for a running AudioContext.
pub fn live_output_frames(context: &Value) -> Option<u64> {
    let context_id = audio_id(context)?;
    LIVE_OUTPUTS.with(|outputs| {
        outputs
            .borrow()
            .get(&context_id)
            .map(|live| live.output.frames())
    })
}

fn realm_audio_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn register_context(context: &Value, clock: &Rc<RefCell<ContextClock>>) {
    CONTEXTS.with(|contexts| {
        contexts
            .borrow_mut()
            .push((context.clone(), Rc::clone(clock)));
    });
}

fn register_callback_target(target: &Value) {
    CALLBACK_TARGETS.with(|targets| targets.borrow_mut().push(target.clone()));
}

fn error(name: &str, message: &str) -> Value {
    if name == "TypeError" || name == "RangeError" {
        w3cos_core::error_instance(name, vec![Value::string(message)])
    } else {
        w3cos_core::web::dom_exception_instance(message, name)
    }
}

fn throw(name: &str, message: &str) -> ! {
    w3cos_core::throw_value(error(name, message))
}

fn illegal(name: &str) -> ! {
    throw("TypeError", &format!("Illegal constructor: {name}"))
}

fn option(options: &Value, name: &str, fallback: Value) -> Value {
    if options.is_object() {
        let value = options.get_property(name);
        if !value.is_undefined() {
            return value;
        }
    }
    fallback
}

fn positive_number(value: Value, fallback: f64, name: &str) -> f64 {
    if value.is_undefined() {
        return fallback;
    }
    let number = value.to_number();
    if !number.is_finite() || number <= 0.0 {
        throw(
            "RangeError",
            &format!("{name} must be a positive finite number"),
        );
    }
    number
}

fn float32_array(length: usize) -> Value {
    w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Float32Array"),
        vec![Value::Number(length as f64)],
    )
}

fn iterator(values: Vec<Value>) -> Value {
    Value::array(values).call_method("__w3cos_symbol_iterator", Vec::new())
}

fn event(type_name: &str) -> Value {
    w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(type_name)],
    )
}

fn dispatch(target: &Value, type_name: &str) {
    target.call_method("dispatchEvent", vec![event(type_name)]);
}

fn audio_buffer_value(options: Value) -> Value {
    if !options.is_object() {
        throw("TypeError", "AudioBuffer requires an options object");
    }
    let channel_count = positive_number(
        options.get_property("numberOfChannels"),
        1.0,
        "numberOfChannels",
    ) as usize;
    let length = positive_number(options.get_property("length"), 1.0, "length") as usize;
    let sample_rate = positive_number(options.get_property("sampleRate"), 48_000.0, "sampleRate");
    if channel_count > 32 {
        throw(
            "NotSupportedError",
            "AudioBuffer supports at most 32 channels",
        );
    }
    let channels = Rc::new(
        (0..channel_count)
            .map(|_| float32_array(length))
            .collect::<Vec<_>>(),
    );
    let value = Value::object(HashMap::from([
        (
            "duration".into(),
            Value::Number(length as f64 / sample_rate),
        ),
        ("length".into(), Value::Number(length as f64)),
        (
            "numberOfChannels".into(),
            Value::Number(channel_count as f64),
        ),
        ("sampleRate".into(), Value::Number(sample_rate)),
    ]));
    let get_channels = Rc::clone(&channels);
    value.set_property(
        "getChannelData",
        realm_audio_function(move |_, args| {
            let channel = args.first().map(Value::to_u32).unwrap_or_default() as usize;
            get_channels
                .get(channel)
                .cloned()
                .unwrap_or_else(|| throw("IndexSizeError", "AudioBuffer channel is out of range"))
        }),
    );
    let from_channels = Rc::clone(&channels);
    value.set_property(
        "copyFromChannel",
        realm_audio_function(move |_, args| {
            let destination = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::binary::is_typed_array(&destination) {
                throw(
                    "TypeError",
                    "AudioBuffer.copyFromChannel requires a typed array",
                );
            }
            let channel = args.get(1).map(Value::to_u32).unwrap_or_default() as usize;
            let start = args.get(2).map(Value::to_u32).unwrap_or_default() as usize;
            let Some(source) = from_channels.get(channel) else {
                throw("IndexSizeError", "AudioBuffer channel is out of range");
            };
            let destination_length = destination.get_property("length").to_u32() as usize;
            for index in 0..destination_length.min(length.saturating_sub(start)) {
                destination.set_property(
                    &index.to_string(),
                    source.get_property(&(start + index).to_string()),
                );
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "copyToChannel",
        realm_audio_function(move |_, args| {
            let source = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::binary::is_typed_array(&source) {
                throw(
                    "TypeError",
                    "AudioBuffer.copyToChannel requires a typed array",
                );
            }
            let channel = args.get(1).map(Value::to_u32).unwrap_or_default() as usize;
            let start = args.get(2).map(Value::to_u32).unwrap_or_default() as usize;
            let Some(destination) = channels.get(channel) else {
                throw("IndexSizeError", "AudioBuffer channel is out of range");
            };
            let source_length = source.get_property("length").to_u32() as usize;
            for index in 0..source_length.min(length.saturating_sub(start)) {
                destination.set_property(
                    &(start + index).to_string(),
                    source.get_property(&index.to_string()),
                );
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("AudioBuffer").get_property("prototype"),
    );
    value
}

fn audio_param_value(default: f64, min: f64, max: f64) -> Value {
    let value = Value::object(HashMap::from([
        ("automationRate".into(), Value::string("a-rate")),
        ("defaultValue".into(), Value::Number(default)),
        ("maxValue".into(), Value::Number(max)),
        ("minValue".into(), Value::Number(min)),
        ("value".into(), Value::Number(default)),
    ]));
    for method in [
        "exponentialRampToValueAtTime",
        "linearRampToValueAtTime",
        "setValueAtTime",
        "setTargetAtTime",
    ] {
        value.set_property(
            method,
            realm_audio_function(move |this, args| {
                let next = args.first().map(Value::to_number).unwrap_or_default();
                if !next.is_finite() {
                    throw("TypeError", "AudioParam automation value must be finite");
                }
                if method == "exponentialRampToValueAtTime" && next <= 0.0 {
                    throw(
                        "RangeError",
                        "exponentialRampToValueAtTime requires a positive value",
                    );
                }
                this.set_property("value", Value::Number(next.clamp(min, max)));
                this.clone()
            }),
        );
    }
    value.set_property(
        "setValueCurveAtTime",
        realm_audio_function(move |this, args| {
            let curve = args.first().cloned().unwrap_or(Value::Undefined);
            let values: Vec<Value> = curve.iter().collect();
            if values.is_empty() {
                throw("InvalidStateError", "AudioParam curve must not be empty");
            }
            let next = values.last().map(Value::to_number).unwrap_or(default);
            this.set_property("value", Value::Number(next.clamp(min, max)));
            this.clone()
        }),
    );
    for method in ["cancelAndHoldAtTime", "cancelScheduledValues"] {
        value.set_property(method, realm_audio_function(|this, _| this));
    }
    w3cos_core::class::set_prototype_of(&value, &class_for("AudioParam").get_property("prototype"));
    value
}

fn audio_param_map_value(entries: Vec<(String, Value)>) -> Value {
    let entries = Rc::new(entries);
    let value = Value::object(HashMap::new());
    value.set_property("size", Value::Number(entries.len() as f64));
    for (method, projection) in [("keys", 0_u8), ("values", 1_u8), ("entries", 2_u8)] {
        let method_entries = Rc::clone(&entries);
        value.set_property(
            method,
            realm_audio_function(move |_, _| {
                iterator(
                    method_entries
                        .iter()
                        .map(|(name, param)| match projection {
                            0 => Value::string(name),
                            1 => param.clone(),
                            _ => Value::array(vec![Value::string(name), param.clone()]),
                        })
                        .collect(),
                )
            }),
        );
    }
    let get_entries = Rc::clone(&entries);
    value.set_property(
        "get",
        realm_audio_function(move |_, args| {
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            get_entries
                .iter()
                .find(|(candidate, _)| candidate == &name)
                .map(|(_, value)| value.clone())
                .unwrap_or(Value::Undefined)
        }),
    );
    let has_entries = Rc::clone(&entries);
    value.set_property(
        "has",
        realm_audio_function(move |_, args| {
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            Value::Bool(has_entries.iter().any(|(candidate, _)| candidate == &name))
        }),
    );
    let each_entries = Rc::clone(&entries);
    let each_value = value.clone();
    value.set_property(
        "forEach",
        realm_audio_function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                throw("TypeError", "AudioParamMap.forEach requires a callback");
            }
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            for (name, param) in each_entries.iter() {
                callback.call(
                    this_arg.clone(),
                    vec![param.clone(), Value::string(name), each_value.clone()],
                );
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("AudioParamMap").get_property("prototype"),
    );
    value
}

fn require_context(context: &Value) {
    if !w3cos_core::class::instance_of(context, &class_for("BaseAudioContext")) {
        throw("TypeError", "AudioNode requires a BaseAudioContext");
    }
}

fn audio_node_value(class_name: &'static str, context: Value, inputs: u32, outputs: u32) -> Value {
    require_context(&context);
    let node_id = next_audio_id();
    let context_id = audio_id(&context).unwrap_or(0);
    let connections = Rc::new(RefCell::new(Vec::<Value>::new()));
    let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    value.set_property("__w3cos_audio_id", Value::Number(node_id as f64));
    for (name, property) in [
        ("channelCount", Value::Number(2.0)),
        ("channelCountMode", Value::string("max")),
        ("channelInterpretation", Value::string("speakers")),
        ("context", context),
        ("numberOfInputs", Value::Number(inputs as f64)),
        ("numberOfOutputs", Value::Number(outputs as f64)),
    ] {
        value.set_property(name, property);
    }
    let connect_connections = Rc::clone(&connections);
    value.set_property(
        "connect",
        realm_audio_function(move |this, args| {
            let destination = args.first().cloned().unwrap_or(Value::Undefined);
            let is_node = w3cos_core::class::instance_of(&destination, &class_for("AudioNode"));
            let is_param = w3cos_core::class::instance_of(&destination, &class_for("AudioParam"));
            if !is_node && !is_param {
                throw(
                    "TypeError",
                    "AudioNode.connect destination must be an AudioNode or AudioParam",
                );
            }
            if !connect_connections
                .borrow()
                .iter()
                .any(|existing| existing.strict_eq(&destination))
            {
                connect_connections.borrow_mut().push(destination.clone());
            }
            GRAPH_NODES.with(|nodes| {
                nodes
                    .borrow_mut()
                    .entry(node_id)
                    .or_insert_with(|| GraphEntry {
                        node: this.clone(),
                        context_id,
                        kind: class_name,
                        connections: Rc::clone(&connect_connections),
                    });
            });
            refresh_live(context_id);
            if is_node {
                destination
            } else {
                Value::Undefined
            }
        }),
    );
    let disconnect_connections = Rc::clone(&connections);
    value.set_property(
        "disconnect",
        realm_audio_function(move |_, args| {
            if let Some(destination) = args.first() {
                disconnect_connections
                    .borrow_mut()
                    .retain(|existing| !existing.strict_eq(destination));
            } else {
                disconnect_connections.borrow_mut().clear();
            }
            if disconnect_connections.borrow().is_empty() {
                GRAPH_NODES.with(|nodes| nodes.borrow_mut().remove(&node_id));
            }
            refresh_live(context_id);
            Value::Undefined
        }),
    );
    value.set_property(
        "__w3cos_getter_connectionCount",
        realm_audio_function(move |_, _| Value::Number(connections.borrow().len() as f64)),
    );
    w3cos_core::class::set_prototype_of(&value, &class_for(class_name).get_property("prototype"));
    value
}

fn scheduled_source_value(class_name: &'static str, context: Value) -> Value {
    let value = audio_node_value(class_name, context, 0, 1);
    value.set_property("onended", Value::Null);
    value.set_property("__w3cos_started", Value::Bool(false));
    value.set_property(
        "start",
        realm_audio_function(|this, args| {
            if this.get_property("__w3cos_started").to_bool() {
                throw(
                    "InvalidStateError",
                    "AudioScheduledSourceNode already started",
                );
            }
            let when = args.first().map(Value::to_number).unwrap_or(0.0);
            let when = if when.is_nan() { 0.0 } else { when };
            if when < 0.0 {
                throw("RangeError", "start time must not be negative");
            }
            for (index, name) in ["__w3cos_start_offset", "__w3cos_start_duration"]
                .into_iter()
                .enumerate()
            {
                if let Some(value) = args.get(index + 1).filter(|value| !value.is_undefined()) {
                    let number = value.to_number();
                    if !number.is_finite() || number < 0.0 {
                        throw(
                            "RangeError",
                            "start offset and duration must not be negative",
                        );
                    }
                    this.set_property(name, Value::Number(number));
                }
            }
            this.set_property("__w3cos_started", Value::Bool(true));
            this.set_property("__w3cos_start_time", Value::Number(when));
            if let Some(id) = audio_id(&this) {
                acquire_buffer(&this, id);
            }
            refresh_node_context(&this);
            Value::Undefined
        }),
    );
    value.set_property(
        "stop",
        realm_audio_function(|this, args| {
            if !this.get_property("__w3cos_started").to_bool() {
                throw(
                    "InvalidStateError",
                    "AudioScheduledSourceNode must start before stop",
                );
            }
            let when = args.first().map(Value::to_number).unwrap_or(0.0);
            let when = if when.is_nan() { 0.0 } else { when };
            if when < 0.0 {
                throw("RangeError", "stop time must not be negative");
            }
            this.set_property("__w3cos_stop_time", Value::Number(when));
            refresh_node_context(&this);
            if !this.get_property("__w3cos_ended").to_bool() {
                this.set_property("__w3cos_ended", Value::Bool(true));
                dispatch(&this, "ended");
            }
            Value::Undefined
        }),
    );
    value
}

fn set_param(value: &Value, name: &str, default: f64, min: f64, max: f64) {
    value.set_property(name, audio_param_value(default, min, max));
}

fn node_value(class_name: &'static str, context: Value, options: Value) -> Value {
    let (inputs, outputs) = match class_name {
        "AudioBufferSourceNode"
        | "ConstantSourceNode"
        | "OscillatorNode"
        | "MediaStreamAudioSourceNode" => (0, 1),
        "AudioDestinationNode" => (1, 0),
        "ChannelMergerNode" => (
            option(&options, "numberOfInputs", Value::Number(6.0)).to_u32(),
            1,
        ),
        "ChannelSplitterNode" => (
            1,
            option(&options, "numberOfOutputs", Value::Number(6.0)).to_u32(),
        ),
        _ => (1, 1),
    };
    let value = if matches!(
        class_name,
        "AudioBufferSourceNode" | "ConstantSourceNode" | "OscillatorNode"
    ) {
        scheduled_source_value(class_name, context)
    } else {
        audio_node_value(class_name, context, inputs, outputs)
    };
    match class_name {
        "AnalyserNode" => {
            let fft_size = Rc::new(Cell::new(
                option(&options, "fftSize", Value::Number(2048.0)).to_u32(),
            ));
            let get_fft = Rc::clone(&fft_size);
            value.set_property(
                "__w3cos_getter_fftSize",
                realm_audio_function(move |_, _| Value::Number(get_fft.get() as f64)),
            );
            let get_bins = Rc::clone(&fft_size);
            value.set_property(
                "__w3cos_getter_frequencyBinCount",
                realm_audio_function(move |_, _| Value::Number((get_bins.get() / 2) as f64)),
            );
            value.set_property(
                "__w3cos_setter_fftSize",
                realm_audio_function(move |_, args| {
                    let size = args.first().map(Value::to_u32).unwrap_or_default();
                    if !(32..=32768).contains(&size) || !size.is_power_of_two() {
                        throw(
                            "IndexSizeError",
                            "AnalyserNode.fftSize must be a power of two from 32 to 32768",
                        );
                    }
                    fft_size.set(size);
                    Value::Undefined
                }),
            );
            value.set_property(
                "minDecibels",
                option(&options, "minDecibels", Value::Number(-100.0)),
            );
            value.set_property(
                "maxDecibels",
                option(&options, "maxDecibels", Value::Number(-30.0)),
            );
            value.set_property(
                "smoothingTimeConstant",
                option(&options, "smoothingTimeConstant", Value::Number(0.8)),
            );
            for (method, fill) in [
                ("getByteFrequencyData", 0.0),
                ("getByteTimeDomainData", 128.0),
                ("getFloatFrequencyData", f64::NEG_INFINITY),
                ("getFloatTimeDomainData", 0.0),
            ] {
                value.set_property(
                    method,
                    realm_audio_function(move |_, args| {
                        let array = args.first().cloned().unwrap_or(Value::Undefined);
                        let length = array.get_property("length").to_u32();
                        for index in 0..length {
                            array.set_property(&index.to_string(), Value::Number(fill));
                        }
                        Value::Undefined
                    }),
                );
            }
        }
        "AudioBufferSourceNode" => {
            value.set_property("buffer", option(&options, "buffer", Value::Null));
            set_param(&value, "detune", 0.0, -153_600.0, 153_600.0);
            value.set_property("loop", option(&options, "loop", Value::Bool(false)));
            value.set_property("loopEnd", option(&options, "loopEnd", Value::Number(0.0)));
            value.set_property(
                "loopStart",
                option(&options, "loopStart", Value::Number(0.0)),
            );
            set_param(&value, "playbackRate", 1.0, 0.0, f32::MAX as f64);
        }
        "AudioDestinationNode" => {
            value.set_property("maxChannelCount", Value::Number(2.0));
        }
        "BiquadFilterNode" => {
            set_param(&value, "Q", 1.0, -f32::MAX as f64, f32::MAX as f64);
            set_param(&value, "detune", 0.0, -153_600.0, 153_600.0);
            set_param(&value, "frequency", 350.0, 0.0, 24_000.0);
            set_param(&value, "gain", 0.0, -40.0, 40.0);
            value.set_property("type", option(&options, "type", Value::string("lowpass")));
            install_frequency_response(&value);
        }
        "ConstantSourceNode" => set_param(&value, "offset", 1.0, -f32::MAX as f64, f32::MAX as f64),
        "ConvolverNode" => {
            value.set_property("buffer", option(&options, "buffer", Value::Null));
            value.set_property(
                "normalize",
                Value::Bool(
                    !option(&options, "disableNormalization", Value::Bool(false)).to_bool(),
                ),
            );
        }
        "DelayNode" => set_param(
            &value,
            "delayTime",
            option(&options, "delayTime", Value::Number(0.0)).to_number(),
            0.0,
            option(&options, "maxDelayTime", Value::Number(1.0)).to_number(),
        ),
        "DynamicsCompressorNode" => {
            for (name, default, min, max) in [
                ("attack", 0.003, 0.0, 1.0),
                ("knee", 30.0, 0.0, 40.0),
                ("ratio", 12.0, 1.0, 20.0),
                ("release", 0.25, 0.0, 1.0),
                ("threshold", -24.0, -100.0, 0.0),
            ] {
                set_param(&value, name, default, min, max);
            }
            value.set_property("reduction", Value::Number(0.0));
        }
        "GainNode" => set_param(&value, "gain", 1.0, -f32::MAX as f64, f32::MAX as f64),
        "IIRFilterNode" => install_frequency_response(&value),
        "MediaElementAudioSourceNode" => {
            value.set_property("mediaElement", options.get_property("mediaElement"));
        }
        "MediaStreamAudioDestinationNode" => {
            value.set_property(
                "stream",
                w3cos_core::class::construct(
                    &crate::media_devices_web::media_stream_class(),
                    Vec::new(),
                ),
            );
        }
        "MediaStreamAudioSourceNode" => {
            let stream = options.get_property("mediaStream");
            if !w3cos_core::class::instance_of(
                &stream,
                &crate::media_devices_web::media_stream_class(),
            ) {
                throw(
                    "TypeError",
                    "MediaStreamAudioSourceNode requires a MediaStream",
                );
            }
            if stream
                .call_method("getAudioTracks", vec![])
                .get_property("length")
                .to_number()
                < 1.0
            {
                throw(
                    "InvalidStateError",
                    "MediaStreamAudioSourceNode requires a MediaStream with an audio track",
                );
            }
            value.set_property("mediaStream", stream.clone());
            let track_id = stream
                .call_method("getAudioTracks", vec![])
                .get_property("0")
                .get_property("id")
                .to_js_string();
            if let Some(source) = crate::audio_input::source_for_track(&track_id) {
                value.set_property(
                    "channelCount",
                    Value::Number(f64::from(source.channels.max(1))),
                );
                value.set_property("channelCountMode", Value::string("explicit"));
            }
        }
        "OscillatorNode" => {
            set_param(&value, "detune", 0.0, -153_600.0, 153_600.0);
            set_param(&value, "frequency", 440.0, 0.0, 24_000.0);
            value.set_property("type", option(&options, "type", Value::string("sine")));
            value.set_property(
                "setPeriodicWave",
                realm_audio_function(|this, args| {
                    let wave = args.first().cloned().unwrap_or(Value::Undefined);
                    if !w3cos_core::class::instance_of(&wave, &class_for("PeriodicWave")) {
                        throw("TypeError", "OscillatorNode requires a PeriodicWave");
                    }
                    this.set_property("type", Value::string("custom"));
                    this.set_property("__w3cos_periodic_wave", wave);
                    Value::Undefined
                }),
            );
        }
        "PannerNode" => {
            for (name, default) in [
                ("orientationX", 1.0),
                ("orientationY", 0.0),
                ("orientationZ", 0.0),
                ("positionX", 0.0),
                ("positionY", 0.0),
                ("positionZ", 0.0),
            ] {
                set_param(&value, name, default, -f32::MAX as f64, f32::MAX as f64);
            }
            for (name, property) in [
                ("coneInnerAngle", Value::Number(360.0)),
                ("coneOuterAngle", Value::Number(360.0)),
                ("coneOuterGain", Value::Number(0.0)),
                ("distanceModel", Value::string("inverse")),
                ("maxDistance", Value::Number(10_000.0)),
                ("panningModel", Value::string("equalpower")),
                ("refDistance", Value::Number(1.0)),
                ("rolloffFactor", Value::Number(1.0)),
            ] {
                value.set_property(name, property);
            }
            install_position_methods(&value);
        }
        "ScriptProcessorNode" => {
            value.set_property(
                "bufferSize",
                option(&options, "bufferSize", Value::Number(0.0)),
            );
            value.set_property("onaudioprocess", Value::Null);
        }
        "StereoPannerNode" => set_param(&value, "pan", 0.0, -1.0, 1.0),
        "WaveShaperNode" => {
            value.set_property("curve", option(&options, "curve", Value::Null));
            value.set_property(
                "oversample",
                option(&options, "oversample", Value::string("none")),
            );
        }
        "AudioWorkletNode" => {
            value.set_property("onprocessorerror", Value::Null);
            value.set_property("parameters", audio_param_map_value(Vec::new()));
            let channel = w3cos_core::class::construct(
                &crate::worker_web::message_channel_class(),
                Vec::new(),
            );
            value.set_property("port", channel.get_property("port1"));
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: AudioWorkletNode exposes compatible parameters and a \
                     MessagePort, but processor execution requires an isolated real-time audio adapter"
                );
            });
        }
        _ => {}
    }
    if matches!(
        class_name,
        "AudioBufferSourceNode"
            | "ConstantSourceNode"
            | "OscillatorNode"
            | "ScriptProcessorNode"
            | "AudioWorkletNode"
    ) {
        register_callback_target(&value);
    }
    value
}

fn install_frequency_response(value: &Value) {
    value.set_property(
        "getFrequencyResponse",
        realm_audio_function(|_, args| {
            let frequencies = args.first().cloned().unwrap_or(Value::Undefined);
            let magnitude = args.get(1).cloned().unwrap_or(Value::Undefined);
            let phase = args.get(2).cloned().unwrap_or(Value::Undefined);
            let length = frequencies.get_property("length").to_u32();
            if magnitude.get_property("length").to_u32() < length
                || phase.get_property("length").to_u32() < length
            {
                throw(
                    "InvalidAccessError",
                    "frequency response arrays are too small",
                );
            }
            for index in 0..length {
                magnitude.set_property(&index.to_string(), Value::Number(1.0));
                phase.set_property(&index.to_string(), Value::Number(0.0));
            }
            Value::Undefined
        }),
    );
}

fn install_position_methods(value: &Value) {
    value.set_property(
        "setPosition",
        realm_audio_function(|this, args| {
            for (index, name) in ["positionX", "positionY", "positionZ"].iter().enumerate() {
                this.get_property(name).set_property(
                    "value",
                    Value::Number(args.get(index).map(Value::to_number).unwrap_or_default()),
                );
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "setOrientation",
        realm_audio_function(|this, args| {
            for (index, name) in ["orientationX", "orientationY", "orientationZ"]
                .iter()
                .enumerate()
            {
                this.get_property(name).set_property(
                    "value",
                    Value::Number(args.get(index).map(Value::to_number).unwrap_or_default()),
                );
            }
            Value::Undefined
        }),
    );
}

fn audio_listener_value() -> Value {
    let value = Value::object(HashMap::new());
    for (name, default) in [
        ("forwardX", 0.0),
        ("forwardY", 0.0),
        ("forwardZ", -1.0),
        ("positionX", 0.0),
        ("positionY", 0.0),
        ("positionZ", 0.0),
        ("upX", 0.0),
        ("upY", 1.0),
        ("upZ", 0.0),
    ] {
        value.set_property(
            name,
            audio_param_value(default, -f32::MAX as f64, f32::MAX as f64),
        );
    }
    install_position_methods(&value);
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("AudioListener").get_property("prototype"),
    );
    value
}

fn periodic_wave_value(context: Value, options: Value) -> Value {
    require_context(&context);
    if !options.is_object() {
        throw("TypeError", "PeriodicWave requires coefficient options");
    }
    let value = Value::object(HashMap::new());
    value.set_property("__w3cos_real", options.get_property("real"));
    value.set_property("__w3cos_imag", options.get_property("imag"));
    value.set_property(
        "__w3cos_disable_normalization",
        Value::Bool(options.get_property("disableNormalization").to_bool()),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("PeriodicWave").get_property("prototype"),
    );
    value
}

fn worklet_value() -> Value {
    let value = Value::object(HashMap::new());
    value.set_property(
        "addModule",
        realm_audio_function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: AudioWorklet.addModule requires an isolated real-time \
                     processor realm and is unavailable without a native audio adapter"
                );
            });
            w3cos_core::promise::reject(vec![error(
                "NotSupportedError",
                "AudioWorklet processor modules are unavailable",
            )])
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("AudioWorklet").get_property("prototype"),
    );
    value
}

#[derive(Clone)]
struct ContextClock {
    state: &'static str,
    accumulated: f64,
    started_at: f64,
}

fn clock_time(clock: &ContextClock) -> f64 {
    if clock.state == "running" {
        clock.accumulated + (crate::jsdom::performance_now() - clock.started_at) / 1000.0
    } else {
        clock.accumulated
    }
}

fn transition_context(target: &Value, clock: &Rc<RefCell<ContextClock>>, state: &'static str) {
    let now = crate::jsdom::performance_now();
    let mut clock = clock.borrow_mut();
    if clock.state == state {
        return;
    }
    if clock.state == "running" {
        clock.accumulated += (now - clock.started_at) / 1000.0;
    }
    clock.state = state;
    if state == "running" {
        clock.started_at = now;
    }
    drop(clock);
    dispatch(target, "statechange");
}

fn context_value(class_name: &'static str, options: Value) -> Value {
    let (sample_rate, offline_length, offline_channels) = if class_name == "OfflineAudioContext" {
        let (channels, length, rate) = if options.is_object() {
            (
                options.get_property("numberOfChannels"),
                options.get_property("length"),
                options.get_property("sampleRate"),
            )
        } else {
            (Value::Undefined, Value::Undefined, Value::Undefined)
        };
        (
            positive_number(rate, 48_000.0, "sampleRate"),
            positive_number(length, 1.0, "length") as usize,
            positive_number(channels, 1.0, "numberOfChannels") as usize,
        )
    } else {
        (
            positive_number(
                option(&options, "sampleRate", Value::Number(48_000.0)),
                48_000.0,
                "sampleRate",
            ),
            0,
            0,
        )
    };
    let context =
        w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    w3cos_core::class::set_prototype_of(&context, &class_for(class_name).get_property("prototype"));
    let context_id = next_audio_id();
    context.set_property("__w3cos_audio_id", Value::Number(context_id as f64));
    let clock = Rc::new(RefCell::new(ContextClock {
        state: "suspended",
        accumulated: 0.0,
        started_at: crate::jsdom::performance_now(),
    }));
    let current_clock = Rc::clone(&clock);
    context.set_property(
        "__w3cos_getter_currentTime",
        realm_audio_function(move |_, _| Value::Number(clock_time(&current_clock.borrow()))),
    );
    let state_clock = Rc::clone(&clock);
    context.set_property(
        "__w3cos_getter_state",
        realm_audio_function(move |_, _| Value::string(state_clock.borrow().state)),
    );
    context.set_property("sampleRate", Value::Number(sample_rate));
    context.set_property("onstatechange", Value::Null);
    context.set_property("listener", audio_listener_value());
    context.set_property("audioWorklet", worklet_value());
    let destination = node_value("AudioDestinationNode", context.clone(), Value::Undefined);
    let destination_id = audio_id(&destination).unwrap_or(0);
    context.set_property(
        "__w3cos_audio_destination_id",
        Value::Number(destination_id as f64),
    );
    context.set_property("destination", destination);
    install_context_factories(&context);

    if class_name == "AudioContext" {
        for (name, property) in [
            ("baseLatency", Value::Number(0.0)),
            ("onerror", Value::Null),
            ("onsinkchange", Value::Null),
            ("outputLatency", Value::Number(0.0)),
            (
                "playbackStats",
                crate::media_devices_web::media_stats_value("AudioPlaybackStats"),
            ),
            ("sinkId", Value::string("")),
        ] {
            context.set_property(name, property);
        }
        let resume_clock = Rc::clone(&clock);
        context.set_property(
            "resume",
            realm_audio_function(move |this, _| {
                if resume_clock.borrow().state == "closed" {
                    return w3cos_core::promise::reject(vec![error(
                        "InvalidStateError",
                        "Cannot resume a closed AudioContext",
                    )]);
                }
                let start_time = clock_time(&resume_clock.borrow());
                transition_context(&this, &resume_clock, "running");
                open_live_output(&this, sample_rate, start_time);
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        let suspend_clock = Rc::clone(&clock);
        context.set_property(
            "suspend",
            realm_audio_function(move |this, _| {
                if suspend_clock.borrow().state == "closed" {
                    return w3cos_core::promise::reject(vec![error(
                        "InvalidStateError",
                        "Cannot suspend a closed AudioContext",
                    )]);
                }
                transition_context(&this, &suspend_clock, "suspended");
                if let Some(id) = audio_id(&this) {
                    LIVE_OUTPUTS.with(|outputs| {
                        if let Some(live) = outputs.borrow().get(&id) {
                            live.output.set_paused(true);
                        }
                    });
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        let close_clock = Rc::clone(&clock);
        context.set_property(
            "close",
            realm_audio_function(move |this, _| {
                transition_context(&this, &close_clock, "closed");
                if let Some(id) = audio_id(&this) {
                    let live = LIVE_OUTPUTS.with(|outputs| outputs.borrow_mut().remove(&id));
                    drop(live);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        context.set_property(
            "setSinkId",
            realm_audio_function(|_, _| {
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: AudioContext.setSinkId requires a host audio-output \
                         chooser and device adapter"
                    );
                });
                w3cos_core::promise::reject(vec![error(
                    "NotSupportedError",
                    "Audio output device selection is unavailable",
                )])
            }),
        );
        context.set_property(
            "getOutputTimestamp",
            realm_audio_function(|this, _| {
                Value::object(HashMap::from([
                    ("contextTime".into(), this.get_property("currentTime")),
                    (
                        "performanceTime".into(),
                        Value::Number(crate::jsdom::performance_now()),
                    ),
                ]))
            }),
        );
    } else {
        context.set_property("length", Value::Number(offline_length as f64));
        context.set_property("oncomplete", Value::Null);
        let render_clock = Rc::clone(&clock);
        context.set_property(
            "startRendering",
            realm_audio_function(move |this, _| {
                if render_clock.borrow().state != "suspended" {
                    return w3cos_core::promise::reject(vec![error(
                        "InvalidStateError",
                        "OfflineAudioContext can only render once",
                    )]);
                }
                transition_context(&this, &render_clock, "running");
                let buffer = audio_buffer_value(Value::object(HashMap::from([
                    (
                        "numberOfChannels".into(),
                        Value::Number(offline_channels as f64),
                    ),
                    ("length".into(), Value::Number(offline_length as f64)),
                    ("sampleRate".into(), Value::Number(sample_rate)),
                ])));
                render_offline(
                    &this,
                    &buffer,
                    sample_rate,
                    offline_channels,
                    offline_length,
                );
                render_clock.borrow_mut().accumulated = offline_length as f64 / sample_rate;
                transition_context(&this, &render_clock, "closed");
                let completion = w3cos_core::class::construct(
                    &crate::web_events::event_subclass_class("OfflineAudioCompletionEvent"),
                    vec![Value::string("complete")],
                );
                completion.set_property("renderedBuffer", buffer.clone());
                this.call_method("dispatchEvent", vec![completion]);
                w3cos_core::promise::resolve(vec![buffer])
            }),
        );
        context.set_property(
            "resume",
            realm_audio_function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
        );
        context.set_property(
            "suspend",
            realm_audio_function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
        );
    }
    // BaseAudioContext: both live and offline contexts can pull a microphone.
    install_media_factories(&context);
    register_context(&context, &clock);
    context
}

/// Renders the connected graph into the OfflineAudioContext result buffer and
/// fires `ended` for buffer sources that reached their natural end.
fn render_offline(
    context: &Value,
    buffer: &Value,
    sample_rate: f64,
    channels: usize,
    length: usize,
) {
    let (Some(context_id), destination) = (
        audio_id(context),
        context.get_property("__w3cos_audio_destination_id"),
    ) else {
        return;
    };
    let snapshot = graph_snapshot(context_id, destination.to_number() as u64);
    let mut renderer = crate::audio_render::Renderer::new(sample_rate, channels, 0.0);
    let rendered = renderer.render_planar(&snapshot, length);
    for (channel, samples) in rendered.iter().enumerate() {
        let data = buffer.call_method("getChannelData", vec![Value::Number(channel as f64)]);
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        w3cos_core::binary::fill_array_buffer_view(&data, &bytes);
    }
    for id in renderer.finished_sources() {
        let node =
            GRAPH_NODES.with(|nodes| nodes.borrow().get(&id).map(|entry| entry.node.clone()));
        if let Some(node) = node {
            if !node.get_property("__w3cos_ended").to_bool() {
                node.set_property("__w3cos_ended", Value::Bool(true));
                dispatch(&node, "ended");
            }
        }
    }
}

fn install_context_factories(context: &Value) {
    for (method, class_name) in [
        ("createAnalyser", "AnalyserNode"),
        ("createBiquadFilter", "BiquadFilterNode"),
        ("createBufferSource", "AudioBufferSourceNode"),
        ("createChannelMerger", "ChannelMergerNode"),
        ("createChannelSplitter", "ChannelSplitterNode"),
        ("createConstantSource", "ConstantSourceNode"),
        ("createConvolver", "ConvolverNode"),
        ("createDelay", "DelayNode"),
        ("createDynamicsCompressor", "DynamicsCompressorNode"),
        ("createGain", "GainNode"),
        ("createOscillator", "OscillatorNode"),
        ("createPanner", "PannerNode"),
        ("createScriptProcessor", "ScriptProcessorNode"),
        ("createStereoPanner", "StereoPannerNode"),
        ("createWaveShaper", "WaveShaperNode"),
    ] {
        context.set_property(
            method,
            realm_audio_function(move |this, args| {
                let options = match method {
                    "createChannelMerger" => Value::object(HashMap::from([(
                        "numberOfInputs".into(),
                        args.first().cloned().unwrap_or(Value::Number(6.0)),
                    )])),
                    "createChannelSplitter" => Value::object(HashMap::from([(
                        "numberOfOutputs".into(),
                        args.first().cloned().unwrap_or(Value::Number(6.0)),
                    )])),
                    "createDelay" => Value::object(HashMap::from([(
                        "maxDelayTime".into(),
                        args.first().cloned().unwrap_or(Value::Number(1.0)),
                    )])),
                    "createScriptProcessor" => Value::object(HashMap::from([(
                        "bufferSize".into(),
                        args.first().cloned().unwrap_or(Value::Number(0.0)),
                    )])),
                    _ => Value::Undefined,
                };
                node_value(class_name, this, options)
            }),
        );
    }
    context.set_property(
        "createBuffer",
        realm_audio_function(|_, args| {
            audio_buffer_value(Value::object(HashMap::from([
                (
                    "numberOfChannels".into(),
                    args.first().cloned().unwrap_or(Value::Number(1.0)),
                ),
                (
                    "length".into(),
                    args.get(1).cloned().unwrap_or(Value::Number(1.0)),
                ),
                (
                    "sampleRate".into(),
                    args.get(2).cloned().unwrap_or(Value::Number(48_000.0)),
                ),
            ])))
        }),
    );
    context.set_property(
        "createIIRFilter",
        realm_audio_function(|this, args| {
            let feedforward = args.first().cloned().unwrap_or(Value::Undefined);
            let feedback = args.get(1).cloned().unwrap_or(Value::Undefined);
            if feedforward.iter().next().is_none() || feedback.iter().next().is_none() {
                throw(
                    "NotSupportedError",
                    "IIRFilter coefficients must not be empty",
                );
            }
            node_value(
                "IIRFilterNode",
                this,
                Value::object(HashMap::from([
                    ("feedforward".into(), feedforward),
                    ("feedback".into(), feedback),
                ])),
            )
        }),
    );
    context.set_property(
        "createPeriodicWave",
        realm_audio_function(|this, args| {
            periodic_wave_value(
                this,
                Value::object(HashMap::from([
                    (
                        "real".into(),
                        args.first().cloned().unwrap_or(Value::Undefined),
                    ),
                    (
                        "imag".into(),
                        args.get(1).cloned().unwrap_or(Value::Undefined),
                    ),
                ])),
            )
        }),
    );
    context.set_property(
        "decodeAudioData",
        realm_audio_function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: BaseAudioContext.decodeAudioData requires a native \
                     compressed-audio decoder adapter"
                );
            });
            w3cos_core::promise::reject(vec![error(
                "EncodingError",
                "Compressed audio decoding is unavailable",
            )])
        }),
    );
}

fn install_media_factories(context: &Value) {
    for (method, class_name, option_name) in [
        (
            "createMediaElementSource",
            "MediaElementAudioSourceNode",
            "mediaElement",
        ),
        (
            "createMediaStreamSource",
            "MediaStreamAudioSourceNode",
            "mediaStream",
        ),
    ] {
        context.set_property(
            method,
            realm_audio_function(move |this, args| {
                node_value(
                    class_name,
                    this,
                    Value::object(HashMap::from([(
                        option_name.into(),
                        args.first().cloned().unwrap_or(Value::Undefined),
                    )])),
                )
            }),
        );
    }
    context.set_property(
        "createMediaStreamDestination",
        realm_audio_function(|this, _| {
            node_value("MediaStreamAudioDestinationNode", this, Value::Undefined)
        }),
    );
}

fn node_constructor(name: &'static str, args: Vec<Value>) -> Value {
    let context = args.first().cloned().unwrap_or(Value::Undefined);
    if name == "AudioWorkletNode" {
        let processor_name = args.get(1).map(Value::to_js_string).unwrap_or_default();
        if processor_name.is_empty() || processor_name == "undefined" {
            throw("TypeError", "AudioWorkletNode requires a processor name");
        }
        let value = node_value(
            name,
            context,
            args.get(2).cloned().unwrap_or(Value::Undefined),
        );
        value.set_property("__w3cos_processor_name", Value::string(&processor_name));
        return value;
    }
    node_value(
        name,
        context,
        args.get(1).cloned().unwrap_or(Value::Undefined),
    )
}

fn build_class(name: &'static str) -> Value {
    let class = match name {
        "AudioBuffer" => realm_audio_function(|_, args| {
            audio_buffer_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        "AudioContext" => realm_audio_function(|_, args| {
            context_value(
                "AudioContext",
                args.first().cloned().unwrap_or(Value::Undefined),
            )
        }),
        "OfflineAudioContext" => realm_audio_function(|_, args| {
            let options = if args.first().is_some_and(Value::is_object) {
                args[0].clone()
            } else {
                Value::object(HashMap::from([
                    (
                        "numberOfChannels".into(),
                        args.first().cloned().unwrap_or(Value::Number(1.0)),
                    ),
                    (
                        "length".into(),
                        args.get(1).cloned().unwrap_or(Value::Number(1.0)),
                    ),
                    (
                        "sampleRate".into(),
                        args.get(2).cloned().unwrap_or(Value::Number(48_000.0)),
                    ),
                ]))
            };
            context_value("OfflineAudioContext", options)
        }),
        "PeriodicWave" => realm_audio_function(|_, args| {
            periodic_wave_value(
                args.first().cloned().unwrap_or(Value::Undefined),
                args.get(1).cloned().unwrap_or(Value::Undefined),
            )
        }),
        name if is_constructible_node(name) => {
            realm_audio_function(move |_, args| node_constructor(name, args))
        }
        _ => realm_audio_function(move |_, _| illegal(name)),
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in prototype_members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    if let Some(parent) = parent_name(name) {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &class_for(parent).get_property("prototype"),
        );
    }
    class.set_property("prototype", prototype);
    class
}

fn is_constructible_node(name: &str) -> bool {
    matches!(
        name,
        "AnalyserNode"
            | "AudioBufferSourceNode"
            | "AudioWorkletNode"
            | "BiquadFilterNode"
            | "ChannelMergerNode"
            | "ChannelSplitterNode"
            | "ConstantSourceNode"
            | "ConvolverNode"
            | "DelayNode"
            | "DynamicsCompressorNode"
            | "GainNode"
            | "IIRFilterNode"
            | "MediaElementAudioSourceNode"
            | "MediaStreamAudioDestinationNode"
            | "MediaStreamAudioSourceNode"
            | "OscillatorNode"
            | "PannerNode"
            | "StereoPannerNode"
            | "WaveShaperNode"
    )
}

fn parent_name(name: &str) -> Option<&'static str> {
    match name {
        "AudioContext" | "OfflineAudioContext" => Some("BaseAudioContext"),
        "AudioNode" | "BaseAudioContext" => Some("EventTarget"),
        "AudioScheduledSourceNode" => Some("AudioNode"),
        "AudioBufferSourceNode" | "ConstantSourceNode" | "OscillatorNode" => {
            Some("AudioScheduledSourceNode")
        }
        "AnalyserNode"
        | "AudioDestinationNode"
        | "AudioWorkletNode"
        | "BiquadFilterNode"
        | "ChannelMergerNode"
        | "ChannelSplitterNode"
        | "ConvolverNode"
        | "DelayNode"
        | "DynamicsCompressorNode"
        | "GainNode"
        | "IIRFilterNode"
        | "MediaElementAudioSourceNode"
        | "MediaStreamAudioDestinationNode"
        | "MediaStreamAudioSourceNode"
        | "PannerNode"
        | "ScriptProcessorNode"
        | "StereoPannerNode"
        | "WaveShaperNode" => Some("AudioNode"),
        "AudioWorklet" => Some("Worklet"),
        _ => None,
    }
}

fn prototype_members(name: &str) -> &'static [&'static str] {
    match name {
        "AnalyserNode" => &[
            "fftSize",
            "frequencyBinCount",
            "getByteFrequencyData",
            "getByteTimeDomainData",
            "getFloatFrequencyData",
            "getFloatTimeDomainData",
            "maxDecibels",
            "minDecibels",
            "smoothingTimeConstant",
        ],
        "AudioBuffer" => &[
            "copyFromChannel",
            "copyToChannel",
            "duration",
            "getChannelData",
            "length",
            "numberOfChannels",
            "sampleRate",
        ],
        "AudioBufferSourceNode" => &[
            "buffer",
            "detune",
            "loop",
            "loopEnd",
            "loopStart",
            "playbackRate",
            "start",
        ],
        "AudioContext" => &[
            "baseLatency",
            "close",
            "createMediaElementSource",
            "createMediaStreamDestination",
            "createMediaStreamSource",
            "getOutputTimestamp",
            "onerror",
            "onsinkchange",
            "outputLatency",
            "playbackStats",
            "resume",
            "setSinkId",
            "sinkId",
            "suspend",
        ],
        "AudioDestinationNode" => &["maxChannelCount"],
        "AudioListener" => &[
            "forwardX",
            "forwardY",
            "forwardZ",
            "positionX",
            "positionY",
            "positionZ",
            "setOrientation",
            "setPosition",
            "upX",
            "upY",
            "upZ",
        ],
        "AudioNode" => &[
            "channelCount",
            "channelCountMode",
            "channelInterpretation",
            "connect",
            "context",
            "disconnect",
            "numberOfInputs",
            "numberOfOutputs",
        ],
        "AudioParam" => &[
            "automationRate",
            "cancelAndHoldAtTime",
            "cancelScheduledValues",
            "defaultValue",
            "exponentialRampToValueAtTime",
            "linearRampToValueAtTime",
            "maxValue",
            "minValue",
            "setTargetAtTime",
            "setValueAtTime",
            "setValueCurveAtTime",
            "value",
        ],
        "AudioParamMap" => &["entries", "forEach", "get", "has", "keys", "size", "values"],
        "AudioScheduledSourceNode" => &["onended", "start", "stop"],
        "AudioSinkInfo" => &["type"],
        "AudioWorkletNode" => &["onprocessorerror", "parameters", "port"],
        "BaseAudioContext" => &[
            "audioWorklet",
            "createAnalyser",
            "createBiquadFilter",
            "createBuffer",
            "createBufferSource",
            "createChannelMerger",
            "createChannelSplitter",
            "createConstantSource",
            "createConvolver",
            "createDelay",
            "createDynamicsCompressor",
            "createGain",
            "createIIRFilter",
            "createOscillator",
            "createPanner",
            "createPeriodicWave",
            "createScriptProcessor",
            "createStereoPanner",
            "createWaveShaper",
            "currentTime",
            "decodeAudioData",
            "destination",
            "listener",
            "onstatechange",
            "sampleRate",
            "state",
        ],
        "BiquadFilterNode" => &[
            "Q",
            "detune",
            "frequency",
            "gain",
            "getFrequencyResponse",
            "type",
        ],
        "ConstantSourceNode" => &["offset"],
        "ConvolverNode" => &["buffer", "normalize"],
        "DelayNode" => &["delayTime"],
        "DynamicsCompressorNode" => &[
            "attack",
            "knee",
            "ratio",
            "reduction",
            "release",
            "threshold",
        ],
        "GainNode" => &["gain"],
        "IIRFilterNode" => &["getFrequencyResponse"],
        "MediaElementAudioSourceNode" => &["mediaElement"],
        "MediaStreamAudioDestinationNode" => &["stream"],
        "MediaStreamAudioSourceNode" => &["mediaStream"],
        "OfflineAudioContext" => &[
            "length",
            "oncomplete",
            "resume",
            "startRendering",
            "suspend",
        ],
        "OscillatorNode" => &["detune", "frequency", "setPeriodicWave", "type"],
        "PannerNode" => &[
            "coneInnerAngle",
            "coneOuterAngle",
            "coneOuterGain",
            "distanceModel",
            "maxDistance",
            "orientationX",
            "orientationY",
            "orientationZ",
            "panningModel",
            "positionX",
            "positionY",
            "positionZ",
            "refDistance",
            "rolloffFactor",
            "setOrientation",
            "setPosition",
        ],
        "ScriptProcessorNode" => &["bufferSize", "onaudioprocess"],
        "StereoPannerNode" => &["pan"],
        "WaveShaperNode" => &["curve", "oversample"],
        "Worklet" => &["addModule"],
        _ => &[],
    }
}

pub fn class_for(name: &'static str) -> Value {
    if name == "EventTarget" {
        return crate::web_events::event_target_class();
    }
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

pub fn reset() {
    LIVE_OUTPUTS.with(|outputs| outputs.borrow_mut().clear());
    GRAPH_NODES.with(|nodes| nodes.borrow_mut().clear());
    ACQUIRED_BUFFERS.with(|buffers| buffers.borrow_mut().clear());
    LAST_LIVE_POLL.with(|last| last.set(f64::NEG_INFINITY));
    let callback_targets =
        CALLBACK_TARGETS.with(|targets| std::mem::take(&mut *targets.borrow_mut()));
    for target in callback_targets {
        for callback in ["onended", "onaudioprocess", "onprocessorerror"] {
            if !target.get_property(callback).is_undefined() {
                target.set_property(callback, Value::Null);
            }
        }
        for method in ["connect", "disconnect", "start", "stop"] {
            if !target.get_property(method).is_undefined() {
                target.set_property(method, Value::Undefined);
            }
        }
    }
    let contexts = CONTEXTS.with(|contexts| std::mem::take(&mut *contexts.borrow_mut()));
    for (context, clock) in contexts {
        clock.borrow_mut().state = "closed";
        for callback in ["oncomplete", "onerror", "onsinkchange", "onstatechange"] {
            if !context.get_property(callback).is_undefined() {
                context.set_property(callback, Value::Null);
            }
        }
        for method in ["close", "resume", "startRendering", "suspend"] {
            if !context.get_property(method).is_undefined() {
                context.set_property(method, Value::Undefined);
            }
        }
    }
    CLASSES.with(|classes| classes.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_buffer_preserves_float_channel_data_and_copies_ranges() {
        let buffer = audio_buffer_value(Value::object(HashMap::from([
            ("numberOfChannels".into(), Value::Number(2.0)),
            ("length".into(), Value::Number(4.0)),
            ("sampleRate".into(), Value::Number(8.0)),
        ])));
        let source = w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Float32Array"),
            vec![Value::array(vec![Value::Number(0.25), Value::Number(-0.5)])],
        );
        buffer.call_method(
            "copyToChannel",
            vec![source, Value::Number(1.0), Value::Number(1.0)],
        );
        let destination = float32_array(2);
        buffer.call_method(
            "copyFromChannel",
            vec![destination.clone(), Value::Number(1.0), Value::Number(1.0)],
        );
        assert_eq!(destination.get_property("0").to_number(), 0.25);
        assert_eq!(destination.get_property("1").to_number(), -0.5);
        assert_eq!(buffer.get_property("duration").to_number(), 0.5);
    }

    #[test]
    fn context_graph_params_and_state_are_live() {
        let context = context_value(
            "AudioContext",
            Value::object(HashMap::from([(
                "sampleRate".into(),
                Value::Number(48_000.0),
            )])),
        );
        let oscillator = context.call_method("createOscillator", Vec::new());
        let gain = context.call_method("createGain", Vec::new());
        assert!(
            oscillator
                .call_method("connect", vec![gain.clone()])
                .strict_eq(&gain)
        );
        gain.get_property("gain").call_method(
            "setValueAtTime",
            vec![Value::Number(0.25), Value::Number(0.0)],
        );
        assert_eq!(
            gain.get_property("gain").get_property("value").to_number(),
            0.25
        );
        context.call_method("resume", Vec::new());
        assert_eq!(context.get_property("state").to_js_string(), "running");
        oscillator.call_method("start", Vec::new());
        oscillator.call_method("stop", Vec::new());
        context.call_method("close", Vec::new());
        assert_eq!(context.get_property("state").to_js_string(), "closed");
    }

    #[test]
    fn offline_context_returns_silent_audio_buffer_and_completion_event() {
        let completed = Rc::new(Cell::new(false));
        let completed_for_handler = Rc::clone(&completed);
        let context = context_value(
            "OfflineAudioContext",
            Value::object(HashMap::from([
                ("numberOfChannels".into(), Value::Number(1.0)),
                ("length".into(), Value::Number(8.0)),
                ("sampleRate".into(), Value::Number(8.0)),
            ])),
        );
        context.set_property(
            "oncomplete",
            Value::function(move |_, args| {
                completed_for_handler.set(
                    args.first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .get_property("renderedBuffer")
                        .get_property("length")
                        .to_number()
                        == 8.0,
                );
                Value::Undefined
            }),
        );
        let rendered = Rc::new(RefCell::new(Value::Undefined));
        let rendered_for_callback = Rc::clone(&rendered);
        context
            .call_method("startRendering", Vec::new())
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *rendered_for_callback.borrow_mut() =
                        args.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert!(completed.get());
        assert_eq!(rendered.borrow().get_property("duration").to_number(), 1.0);
        assert_eq!(context.get_property("state").to_js_string(), "closed");
    }

    fn offline_context(channels: f64, length: f64, rate: f64) -> Value {
        context_value(
            "OfflineAudioContext",
            Value::object(HashMap::from([
                ("numberOfChannels".into(), Value::Number(channels)),
                ("length".into(), Value::Number(length)),
                ("sampleRate".into(), Value::Number(rate)),
            ])),
        )
    }

    fn render(context: &Value) -> Value {
        let rendered = Rc::new(RefCell::new(Value::Undefined));
        let slot = Rc::clone(&rendered);
        context
            .call_method("startRendering", Vec::new())
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *slot.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        let buffer = rendered.borrow().clone();
        assert!(!buffer.is_undefined(), "startRendering did not resolve");
        buffer
    }

    fn channel(buffer: &Value, index: usize) -> Vec<f32> {
        let data = buffer.call_method("getChannelData", vec![Value::Number(index as f64)]);
        w3cos_core::binary::bytes_of(&data)
            .unwrap()
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect()
    }

    #[test]
    fn offline_render_plays_oscillator_through_gain_into_destination() {
        let context = offline_context(1.0, 256.0, 8_000.0);
        {
            // The graph stays alive through its connections, like a browser.
            let oscillator = context.call_method("createOscillator", Vec::new());
            let gain = context.call_method("createGain", Vec::new());
            oscillator
                .get_property("frequency")
                .set_property("value", Value::Number(1_000.0));
            gain.get_property("gain")
                .set_property("value", Value::Number(0.5));
            oscillator.call_method("connect", vec![gain.clone()]);
            gain.call_method("connect", vec![context.get_property("destination")]);
            oscillator.call_method("start", vec![Value::Number(0.0)]);
        }
        let samples = channel(&render(&context), 0);
        assert_eq!(samples.len(), 256);
        for (index, sample) in samples.iter().enumerate() {
            let expected =
                0.5 * (2.0 * std::f64::consts::PI * 1_000.0 * index as f64 / 8_000.0).sin();
            assert!(
                (*sample as f64 - expected).abs() < 1e-4,
                "frame {index}: {sample}"
            );
        }
    }

    #[test]
    fn offline_render_schedules_buffer_sources_and_fires_natural_ended() {
        let context = offline_context(2.0, 8.0, 8.0);
        let buffer = context.call_method(
            "createBuffer",
            vec![Value::Number(1.0), Value::Number(2.0), Value::Number(8.0)],
        );
        let data = buffer.call_method("getChannelData", vec![Value::Number(0.0)]);
        data.set_property("0", Value::Number(0.25));
        data.set_property("1", Value::Number(-0.5));
        let source = context.call_method("createBufferSource", Vec::new());
        source.set_property("buffer", buffer);
        source.call_method("connect", vec![context.get_property("destination")]);
        let ended = Rc::new(Cell::new(0));
        let ended_count = Rc::clone(&ended);
        source.call_method(
            "addEventListener",
            vec![
                Value::string("ended"),
                Value::function(move |_, _| {
                    ended_count.set(ended_count.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        source.call_method("start", vec![Value::Number(0.5)]);
        let rendered = render(&context);
        let expected = vec![0.0, 0.0, 0.0, 0.0, 0.25, -0.5, 0.0, 0.0];
        // Mono buffer up-mixes to both destination channels.
        assert_eq!(channel(&rendered, 0), expected);
        assert_eq!(channel(&rendered, 1), expected);
        assert_eq!(ended.get(), 1);
    }

    #[test]
    fn offline_render_pans_and_leaves_unconnected_or_unstarted_sources_silent() {
        let context = offline_context(2.0, 4.0, 8_000.0);
        let constant = context.call_method("createConstantSource", Vec::new());
        let panner = context.call_method("createStereoPanner", Vec::new());
        panner
            .get_property("pan")
            .set_property("value", Value::Number(1.0));
        constant.call_method("connect", vec![panner.clone()]);
        panner.call_method("connect", vec![context.get_property("destination")]);
        constant.call_method("start", Vec::new());
        let idle = context.call_method("createOscillator", Vec::new());
        idle.call_method("start", Vec::new());
        let unstarted = context.call_method("createOscillator", Vec::new());
        unstarted.call_method("connect", vec![context.get_property("destination")]);
        let rendered = render(&context);
        assert!(
            channel(&rendered, 0)
                .iter()
                .all(|sample| sample.abs() < 1e-6)
        );
        assert!(
            channel(&rendered, 1)
                .iter()
                .all(|sample| (sample - 1.0).abs() < 1e-6)
        );
    }

    #[test]
    fn source_start_and_stop_validate_times_and_disconnect_detaches_nodes() {
        let context = offline_context(1.0, 4.0, 4.0);
        let source = context.call_method("createConstantSource", Vec::new());
        let negative =
            w3cos_core::catch_js(|| source.call_method("start", vec![Value::Number(-1.0)]))
                .expect_err("negative start must throw");
        assert_eq!(negative.get_property("name").to_js_string(), "RangeError");
        source.call_method("connect", vec![context.get_property("destination")]);
        source.call_method("start", vec![Value::Number(0.25)]);
        source.call_method("disconnect", Vec::new());
        let rendered = render(&context);
        assert_eq!(channel(&rendered, 0), vec![0.0; 4]);
    }

    #[test]
    fn audio_classes_contexts_callbacks_and_graph_methods_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_context_class = class_for("AudioContext");
        let old_buffer_class = class_for("AudioBuffer");
        assert!(old_context_class.strict_eq(&class_for("AudioContext")));
        let context = w3cos_core::class::construct(&old_context_class, Vec::new());
        let oscillator = context.call_method("createOscillator", Vec::new());
        let buffer = w3cos_core::class::construct(
            &old_buffer_class,
            vec![Value::object(HashMap::from([
                ("numberOfChannels".into(), Value::Number(1.0)),
                ("length".into(), Value::Number(1.0)),
                ("sampleRate".into(), Value::Number(8_000.0)),
            ]))],
        );

        let state_marker = Rc::new(());
        let state_marker_weak = Rc::downgrade(&state_marker);
        context.set_property(
            "onstatechange",
            Value::function(move |_, _| {
                let _ = &state_marker;
                Value::Undefined
            }),
        );
        let ended_marker = Rc::new(());
        let ended_marker_weak = Rc::downgrade(&ended_marker);
        oscillator.set_property(
            "onended",
            Value::function(move |_, _| {
                let _ = &ended_marker;
                Value::Undefined
            }),
        );

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_context_class.strict_eq(&class_for("AudioContext")));
        assert!(!old_buffer_class.strict_eq(&class_for("AudioBuffer")));
        for class in [old_context_class, old_buffer_class] {
            assert!(class.call(Value::Undefined, Vec::new()).is_undefined());
        }
        assert!(context.call_method("resume", Vec::new()).is_undefined());
        assert!(context.get_property("onstatechange").is_undefined());
        assert!(oscillator.call_method("start", Vec::new()).is_undefined());
        assert!(oscillator.get_property("onended").is_undefined());
        assert!(
            buffer
                .call_method("getChannelData", vec![Value::Number(0.0)])
                .is_undefined()
        );
        assert!(state_marker_weak.upgrade().is_none());
        assert!(ended_marker_weak.upgrade().is_none());
    }

    fn promise_value(promise: Value) -> Result<Value, Value> {
        let result = Rc::new(RefCell::new(None));
        let fulfilled = Rc::clone(&result);
        let rejected = Rc::clone(&result);
        promise.call_method(
            "then",
            vec![
                Value::function(move |_, args| {
                    *fulfilled.borrow_mut() = Some(Ok(args.first().cloned().unwrap_or_default()));
                    Value::Undefined
                }),
                Value::function(move |_, args| {
                    *rejected.borrow_mut() = Some(Err(args.first().cloned().unwrap_or_default()));
                    Value::Undefined
                }),
            ],
        );
        crate::jsdom::drain_microtasks();
        result.borrow().clone().unwrap_or(Err(Value::Undefined))
    }

    #[test]
    fn microphone_pcm_reaches_an_offline_audio_context_through_gain() {
        crate::media_devices_web::reset();
        crate::capability_gate::reset();
        let mic = crate::audio_input::MicrophoneInfo {
            label: "Graph Microphone".into(),
            device_key: "audio-web:mic".into(),
            group_key: "audio-web:group".into(),
            sample_rate: 8_000,
            channels: 1,
            is_default: true,
        };
        crate::audio_input::set_microphone_override(Some(vec![mic.clone()]));
        crate::audio_input::push_microphone_samples(&mic.device_key, &[0.25, -0.5, 0.125, -0.125]);
        crate::capability_gate::grant("microphone");
        let stream = promise_value(crate::media_devices_web::media_devices_value().call_method(
            "getUserMedia",
            vec![Value::object(HashMap::from([(
                "audio".into(),
                Value::Bool(true),
            )]))],
        ))
        .expect("getUserMedia");
        let context = offline_context(1.0, 4.0, 8_000.0);
        let source = context.call_method("createMediaStreamSource", vec![stream.clone()]);
        assert_eq!(source.get_property("channelCount").to_number(), 1.0);
        assert_eq!(source.get_property("numberOfInputs").to_number(), 0.0);
        let gain = context.call_method("createGain", Vec::new());
        gain.get_property("gain")
            .set_property("value", Value::Number(0.5));
        source.call_method("connect", vec![gain.clone()]);
        gain.call_method("connect", vec![context.get_property("destination")]);
        assert_eq!(
            channel(&render(&context), 0),
            vec![0.125, -0.25, 0.0625, -0.0625]
        );

        let track = stream
            .call_method("getAudioTracks", vec![])
            .get_property("0");
        track.set_property("enabled", Value::Bool(false));
        let muted = offline_context(1.0, 4.0, 8_000.0);
        let muted_source = muted.call_method("createMediaStreamSource", vec![stream.clone()]);
        muted_source.call_method("connect", vec![muted.get_property("destination")]);
        assert!(
            channel(&render(&muted), 0)
                .iter()
                .all(|sample| sample.abs() < 1e-6)
        );

        let video =
            crate::media_devices_web::stream_value(vec![crate::media_devices_web::track_value(
                "video", "camera",
            )]);
        let fresh = offline_context(1.0, 4.0, 8_000.0);
        let error =
            w3cos_core::catch_js(|| fresh.call_method("createMediaStreamSource", vec![video]))
                .expect_err("a video-only stream is not a media-stream source");
        assert_eq!(
            error.get_property("name").to_js_string(),
            "InvalidStateError"
        );

        crate::capability_gate::reset();
        crate::media_devices_web::reset();
    }
}
