//! Web Audio graph, buffer and lifecycle compatibility layer.
//!
//! The graph and offline zero-signal rendering are implemented locally. A
//! host audio device/decoder/worklet adapter is still required for audible
//! real-time output, compressed decoding and processor module execution.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

use crate::jsdom::realm_function;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static CONTEXTS: RefCell<Vec<(Value, Rc<RefCell<ContextClock>>)>> =
        const { RefCell::new(Vec::new()) };
    static CALLBACK_TARGETS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
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
    let connections = Rc::new(RefCell::new(Vec::<Value>::new()));
    let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
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
        realm_audio_function(move |_, args| {
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
        realm_audio_function(|this, _| {
            if this.get_property("__w3cos_started").to_bool() {
                throw(
                    "InvalidStateError",
                    "AudioScheduledSourceNode already started",
                );
            }
            this.set_property("__w3cos_started", Value::Bool(true));
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: Web Audio source scheduling and graph state are active, \
                     but audible real-time output requires a native audio device adapter"
                );
            });
            Value::Undefined
        }),
    );
    value.set_property(
        "stop",
        realm_audio_function(|this, _| {
            if !this.get_property("__w3cos_started").to_bool() {
                throw(
                    "InvalidStateError",
                    "AudioScheduledSourceNode must start before stop",
                );
            }
            dispatch(&this, "ended");
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
        "AudioBufferSourceNode" | "ConstantSourceNode" | "OscillatorNode" => (0, 1),
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
            value.set_property("mediaStream", options.get_property("mediaStream"));
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
    context.set_property(
        "destination",
        node_value("AudioDestinationNode", context.clone(), Value::Undefined),
    );
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
                transition_context(&this, &resume_clock, "running");
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: AudioContext graph time can run, but audible output \
                         requires a native audio device adapter"
                    );
                });
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
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        let close_clock = Rc::clone(&clock);
        context.set_property(
            "close",
            realm_audio_function(move |this, _| {
                transition_context(&this, &close_clock, "closed");
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
        install_media_factories(&context);
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
    register_context(&context, &clock);
    context
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
}
