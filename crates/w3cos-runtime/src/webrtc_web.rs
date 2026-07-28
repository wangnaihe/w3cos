//! WebRTC object model and local signaling compatibility layer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
}

fn realm_rtc_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn register_rtc_value(value: &Value) {
    register_weak_realm_object(&VALUES, value);
}

const NAMES: &[&str] = &[
    "RTCCertificate",
    "RTCDTMFSender",
    "RTCDataChannel",
    "RTCDtlsTransport",
    "RTCEncodedAudioFrame",
    "RTCEncodedVideoFrame",
    "RTCError",
    "RTCIceCandidate",
    "RTCIceTransport",
    "RTCPeerConnection",
    "RTCRtpReceiver",
    "RTCRtpScriptTransform",
    "RTCRtpSender",
    "RTCRtpTransceiver",
    "RTCSctpTransport",
    "RTCSessionDescription",
    "RTCStatsReport",
];

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

fn event(type_name: &str) -> Value {
    w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(type_name)],
    )
}

fn dispatch(target: &Value, type_name: &str) {
    target.call_method("dispatchEvent", vec![event(type_name)]);
}

fn copy_fields(source: &Value, names: &[&str]) -> Value {
    Value::object(
        names
            .iter()
            .map(|name| ((*name).to_string(), source.get_property(name)))
            .collect(),
    )
}

fn session_description_value(init: Value) -> Value {
    if !init.is_object() {
        throw("TypeError", "RTCSessionDescription requires an init object");
    }
    let type_name = init.get_property("type").to_js_string();
    if !matches!(
        type_name.as_str(),
        "answer" | "offer" | "pranswer" | "rollback"
    ) {
        throw("TypeError", "RTCSessionDescription type is invalid");
    }
    let sdp = init.get_property("sdp");
    let value = Value::object(HashMap::from([
        (
            "sdp".into(),
            if sdp.is_undefined() {
                Value::string("")
            } else {
                sdp
            },
        ),
        ("type".into(), Value::string(&type_name)),
    ]));
    let json = value.clone();
    value.set_property(
        "toJSON",
        realm_rtc_function(move |_, _| copy_fields(&json, &["type", "sdp"])),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCSessionDescription").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn nullable(init: &Value, name: &str) -> Value {
    let value = init.get_property(name);
    if value.is_undefined() {
        Value::Null
    } else {
        value
    }
}

fn ice_candidate_value(init: Value) -> Value {
    let init = if init.is_object() {
        init
    } else {
        Value::object(HashMap::new())
    };
    let candidate = init.get_property("candidate");
    let value = Value::object(HashMap::from([
        ("address".into(), nullable(&init, "address")),
        (
            "candidate".into(),
            if candidate.is_undefined() {
                Value::string("")
            } else {
                candidate
            },
        ),
        ("component".into(), nullable(&init, "component")),
        ("foundation".into(), nullable(&init, "foundation")),
        ("port".into(), nullable(&init, "port")),
        ("priority".into(), nullable(&init, "priority")),
        ("protocol".into(), nullable(&init, "protocol")),
        ("relatedAddress".into(), nullable(&init, "relatedAddress")),
        ("relatedPort".into(), nullable(&init, "relatedPort")),
        ("relayProtocol".into(), nullable(&init, "relayProtocol")),
        ("sdpMLineIndex".into(), nullable(&init, "sdpMLineIndex")),
        ("sdpMid".into(), nullable(&init, "sdpMid")),
        ("tcpType".into(), nullable(&init, "tcpType")),
        ("type".into(), nullable(&init, "type")),
        ("url".into(), nullable(&init, "url")),
        (
            "usernameFragment".into(),
            nullable(&init, "usernameFragment"),
        ),
    ]));
    let json = value.clone();
    value.set_property(
        "toJSON",
        realm_rtc_function(move |_, _| {
            copy_fields(
                &json,
                &["candidate", "sdpMLineIndex", "sdpMid", "usernameFragment"],
            )
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCIceCandidate").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn rtc_error_value(init: Value, message: Value) -> Value {
    if !init.is_object() {
        throw("TypeError", "RTCError requires an init object");
    }
    let value = error("OperationError", &message.to_js_string());
    for name in [
        "errorDetail",
        "httpRequestStatusCode",
        "receivedAlert",
        "sctpCauseCode",
        "sdpLineNumber",
        "sentAlert",
    ] {
        value.set_property(name, nullable(&init, name));
    }
    w3cos_core::class::set_prototype_of(&value, &class_for("RTCError").get_property("prototype"));
    register_rtc_value(&value);
    value
}

fn stats_report_value(entries: Vec<(String, Value)>) -> Value {
    let entries = Rc::new(entries);
    let value = Value::object(HashMap::new());
    value.set_property("size", Value::Number(entries.len() as f64));
    for (method, projection) in [("keys", 0_u8), ("values", 1_u8), ("entries", 2_u8)] {
        let method_entries = Rc::clone(&entries);
        value.set_property(
            method,
            realm_rtc_function(move |_, _| {
                let values = method_entries
                    .iter()
                    .map(|(key, entry)| match projection {
                        0 => Value::string(key),
                        1 => entry.clone(),
                        _ => Value::array(vec![Value::string(key), entry.clone()]),
                    })
                    .collect();
                Value::array(values).call_method("__w3cos_symbol_iterator", Vec::new())
            }),
        );
    }
    let get_entries = Rc::clone(&entries);
    value.set_property(
        "get",
        realm_rtc_function(move |_, args| {
            let key = args.first().map(Value::to_js_string).unwrap_or_default();
            get_entries
                .iter()
                .find(|(candidate, _)| candidate == &key)
                .map(|(_, entry)| entry.clone())
                .unwrap_or(Value::Undefined)
        }),
    );
    let has_entries = Rc::clone(&entries);
    value.set_property(
        "has",
        realm_rtc_function(move |_, args| {
            let key = args.first().map(Value::to_js_string).unwrap_or_default();
            Value::Bool(has_entries.iter().any(|(candidate, _)| candidate == &key))
        }),
    );
    let each_entries = Rc::clone(&entries);
    let each_value = value.clone();
    value.set_property(
        "forEach",
        realm_rtc_function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                throw("TypeError", "RTCStatsReport.forEach requires a callback");
            }
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            for (key, entry) in each_entries.iter() {
                callback.call(
                    this_arg.clone(),
                    vec![entry.clone(), Value::string(key), each_value.clone()],
                );
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCStatsReport").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn encoded_streams_value() -> Value {
    let transform =
        w3cos_core::class::construct(&crate::streams_web::transform_stream_class(), Vec::new());
    let value = Value::object(HashMap::from([
        ("readable".into(), transform.get_property("readable")),
        ("writable".into(), transform.get_property("writable")),
    ]));
    register_rtc_value(&value);
    value
}

fn dtmf_sender_value(track: Value) -> Value {
    let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    value.set_property("canInsertDTMF", Value::Bool(false));
    value.set_property("ontonechange", Value::Null);
    value.set_property("toneBuffer", Value::string(""));
    value.set_property("__w3cos_track", track);
    value.set_property(
        "insertDTMF",
        realm_rtc_function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: RTCDTMFSender requires a negotiated RTP audio codec and \
                     cannot emit tones without a native WebRTC transport"
                );
            });
            throw(
                "InvalidStateError",
                "DTMF is unavailable without a negotiated RTP transport",
            )
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCDTMFSender").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn data_channel_value(label: String, init: Value) -> Value {
    let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    for (name, property) in [
        ("binaryType", Value::string("arraybuffer")),
        ("bufferedAmount", Value::Number(0.0)),
        ("bufferedAmountLowThreshold", Value::Number(0.0)),
        ("id", nullable(&init, "id")),
        ("label", Value::string(&label)),
        ("maxPacketLifeTime", nullable(&init, "maxPacketLifeTime")),
        ("maxRetransmits", nullable(&init, "maxRetransmits")),
        (
            "negotiated",
            Value::Bool(init.get_property("negotiated").to_bool()),
        ),
        ("onbufferedamountlow", Value::Null),
        ("onclose", Value::Null),
        ("onclosing", Value::Null),
        ("onerror", Value::Null),
        ("onmessage", Value::Null),
        ("onopen", Value::Null),
        (
            "ordered",
            Value::Bool(
                init.get_property("ordered").is_undefined()
                    || init.get_property("ordered").to_bool(),
            ),
        ),
        (
            "protocol",
            if init.get_property("protocol").is_undefined() {
                Value::string("")
            } else {
                init.get_property("protocol")
            },
        ),
        ("readyState", Value::string("connecting")),
        ("reliable", Value::Bool(true)),
    ] {
        value.set_property(name, property);
    }
    value.set_property(
        "send",
        realm_rtc_function(|this, _| {
            if this.get_property("readyState").to_js_string() != "open" {
                throw(
                    "InvalidStateError",
                    "RTCDataChannel is not backed by an open SCTP transport",
                );
            }
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: RTCDataChannel has no native SCTP/DTLS transport; \
                     application data is not reported as delivered"
                );
            });
            throw("OperationError", "RTCDataChannel transport is unavailable")
        }),
    );
    value.set_property(
        "close",
        realm_rtc_function(|this, _| {
            let state = this.get_property("readyState").to_js_string();
            if state == "closed" || state == "closing" {
                return Value::Undefined;
            }
            this.set_property("readyState", Value::string("closing"));
            dispatch(&this, "closing");
            this.set_property("readyState", Value::string("closed"));
            dispatch(&this, "close");
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCDataChannel").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn capabilities(kind: &str) -> Value {
    if !matches!(kind, "audio" | "video") {
        return Value::Null;
    }
    Value::object(HashMap::from([
        ("codecs".into(), Value::array(Vec::new())),
        ("headerExtensions".into(), Value::array(Vec::new())),
    ]))
}

fn rtp_sender_value(track: Value, streams: Vec<Value>) -> Value {
    let value = Value::object(HashMap::from([
        ("dtmf".into(), Value::Null),
        ("rtcpTransport".into(), Value::Null),
        ("track".into(), track.clone()),
        ("transform".into(), Value::Null),
        ("transport".into(), Value::Null),
    ]));
    if track.get_property("kind").to_js_string() == "audio" {
        value.set_property("dtmf", dtmf_sender_value(track));
    }
    value.set_property(
        "__w3cos_parameters",
        Value::object(HashMap::from([
            ("codecs".into(), Value::array(Vec::new())),
            ("encodings".into(), Value::array(Vec::new())),
            ("headerExtensions".into(), Value::array(Vec::new())),
            ("rtcp".into(), Value::object(HashMap::new())),
            ("transactionId".into(), Value::string("")),
        ])),
    );
    value.set_property("__w3cos_streams", Value::array(streams));
    value.set_property(
        "getParameters",
        realm_rtc_function(|this, _| {
            w3cos_core::web::structured_clone(vec![this.get_property("__w3cos_parameters")])
        }),
    );
    value.set_property(
        "setParameters",
        realm_rtc_function(|this, args| {
            let parameters = args.first().cloned().unwrap_or(Value::Undefined);
            if !parameters.is_object() {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    "RTCRtpSender.setParameters requires an object",
                )]);
            }
            this.set_property("__w3cos_parameters", parameters);
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    value.set_property(
        "replaceTrack",
        realm_rtc_function(|this, args| {
            let track = args.first().cloned().unwrap_or(Value::Null);
            if !track.is_null()
                && !w3cos_core::class::instance_of(
                    &track,
                    &crate::media_devices_web::media_stream_track_class(),
                )
            {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    "RTCRtpSender.replaceTrack requires a MediaStreamTrack or null",
                )]);
            }
            this.set_property("track", track);
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    value.set_property(
        "setStreams",
        realm_rtc_function(|this, args| {
            this.set_property("__w3cos_streams", Value::array(args));
            Value::Undefined
        }),
    );
    value.set_property(
        "getStats",
        realm_rtc_function(|_, _| {
            w3cos_core::promise::resolve(vec![stats_report_value(Vec::new())])
        }),
    );
    value.set_property(
        "createEncodedStreams",
        realm_rtc_function(|_, _| encoded_streams_value()),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCRtpSender").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn rtp_receiver_value(track: Value) -> Value {
    let value = Value::object(HashMap::from([
        ("jitterBufferTarget".into(), Value::Null),
        ("playoutDelayHint".into(), Value::Null),
        ("rtcpTransport".into(), Value::Null),
        ("track".into(), track),
        ("transform".into(), Value::Null),
        ("transport".into(), Value::Null),
    ]));
    value.set_property(
        "getParameters",
        realm_rtc_function(|_, _| {
            Value::object(HashMap::from([
                ("codecs".into(), Value::array(Vec::new())),
                ("encodings".into(), Value::array(Vec::new())),
                ("headerExtensions".into(), Value::array(Vec::new())),
                ("rtcp".into(), Value::object(HashMap::new())),
            ]))
        }),
    );
    for method in ["getContributingSources", "getSynchronizationSources"] {
        value.set_property(method, realm_rtc_function(|_, _| Value::array(Vec::new())));
    }
    value.set_property(
        "getStats",
        realm_rtc_function(|_, _| {
            w3cos_core::promise::resolve(vec![stats_report_value(Vec::new())])
        }),
    );
    value.set_property(
        "createEncodedStreams",
        realm_rtc_function(|_, _| encoded_streams_value()),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCRtpReceiver").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn transceiver_value(track: Value, kind: String, init: Value, streams: Vec<Value>) -> Value {
    let sender = rtp_sender_value(track, streams);
    let receiver_track = crate::media_devices_web::track_value(&kind, "remote WebRTC track");
    let receiver = rtp_receiver_value(receiver_track);
    let value = Value::object(HashMap::from([
        ("currentDirection".into(), Value::Null),
        (
            "direction".into(),
            if init.get_property("direction").is_undefined() {
                Value::string("sendrecv")
            } else {
                init.get_property("direction")
            },
        ),
        ("mid".into(), Value::Null),
        ("receiver".into(), receiver),
        ("sender".into(), sender),
        ("stopped".into(), Value::Bool(false)),
    ]));
    value.set_property(
        "setCodecPreferences",
        realm_rtc_function(|this, args| {
            this.set_property(
                "__w3cos_codec_preferences",
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::array(Vec::new())),
            );
            Value::Undefined
        }),
    );
    value.set_property(
        "getHeaderExtensionsToNegotiate",
        realm_rtc_function(|this, _| {
            let extensions = this.get_property("__w3cos_header_extensions");
            if extensions.is_undefined() {
                Value::array(Vec::new())
            } else {
                extensions
            }
        }),
    );
    value.set_property(
        "setHeaderExtensionsToNegotiate",
        realm_rtc_function(|this, args| {
            this.set_property(
                "__w3cos_header_extensions",
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::array(Vec::new())),
            );
            Value::Undefined
        }),
    );
    value.set_property(
        "getNegotiatedHeaderExtensions",
        realm_rtc_function(|_, _| Value::array(Vec::new())),
    );
    value.set_property(
        "stop",
        realm_rtc_function(|this, _| {
            this.set_property("stopped", Value::Bool(true));
            this.set_property("currentDirection", Value::Null);
            this.set_property("direction", Value::string("stopped"));
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCRtpTransceiver").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn minimal_sdp(type_name: &str) -> Value {
    session_description_value(Value::object(HashMap::from([
        ("type".into(), Value::string(type_name)),
        (
            "sdp".into(),
            Value::string("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n"),
        ),
    ])))
}

fn normalize_description(value: Value) -> Value {
    if w3cos_core::class::instance_of(&value, &class_for("RTCSessionDescription")) {
        value
    } else {
        session_description_value(value)
    }
}

fn peer_connection_value(configuration: Value) -> Value {
    let connection =
        w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    let configuration = Rc::new(RefCell::new(if configuration.is_object() {
        configuration
    } else {
        Value::object(HashMap::new())
    }));
    let local_description = Rc::new(RefCell::new(Value::Null));
    let remote_description = Rc::new(RefCell::new(Value::Null));
    let local_streams = Rc::new(RefCell::new(Vec::<Value>::new()));
    let remote_streams = Rc::new(RefCell::new(Vec::<Value>::new()));
    let senders = Rc::new(RefCell::new(Vec::<Value>::new()));
    let receivers = Rc::new(RefCell::new(Vec::<Value>::new()));
    let transceivers = Rc::new(RefCell::new(Vec::<Value>::new()));
    let channels = Rc::new(RefCell::new(Vec::<Value>::new()));
    let candidates = Rc::new(RefCell::new(Vec::<Value>::new()));
    for (name, property) in [
        ("canTrickleIceCandidates", Value::Null),
        ("connectionState", Value::string("new")),
        ("currentLocalDescription", Value::Null),
        ("currentRemoteDescription", Value::Null),
        ("iceConnectionState", Value::string("new")),
        ("iceGatheringState", Value::string("new")),
        ("localDescription", Value::Null),
        ("pendingLocalDescription", Value::Null),
        ("pendingRemoteDescription", Value::Null),
        ("remoteDescription", Value::Null),
        ("sctp", Value::Null),
        ("signalingState", Value::string("stable")),
    ] {
        connection.set_property(name, property);
    }
    for handler in [
        "onaddstream",
        "onconnectionstatechange",
        "ondatachannel",
        "onicecandidate",
        "onicecandidateerror",
        "oniceconnectionstatechange",
        "onicegatheringstatechange",
        "onnegotiationneeded",
        "onremovestream",
        "onsignalingstatechange",
        "ontrack",
    ] {
        connection.set_property(handler, Value::Null);
    }

    connection.set_property(
        "createOffer",
        realm_rtc_function(|this, _| {
            if this.get_property("signalingState").to_js_string() == "closed" {
                return w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "RTCPeerConnection is closed",
                )]);
            }
            w3cos_core::promise::resolve(vec![minimal_sdp("offer")])
        }),
    );
    connection.set_property(
        "createAnswer",
        realm_rtc_function(|this, _| {
            if this.get_property("signalingState").to_js_string() != "have-remote-offer" {
                return w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "createAnswer requires a remote offer",
                )]);
            }
            w3cos_core::promise::resolve(vec![minimal_sdp("answer")])
        }),
    );

    let set_local = Rc::clone(&local_description);
    connection.set_property(
        "setLocalDescription",
        realm_rtc_function(move |this, args| {
            if this.get_property("signalingState").to_js_string() == "closed" {
                return w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "RTCPeerConnection is closed",
                )]);
            }
            let description = if let Some(value) = args.first() {
                normalize_description(value.clone())
            } else {
                minimal_sdp(
                    if this.get_property("signalingState").to_js_string() == "have-remote-offer" {
                        "answer"
                    } else {
                        "offer"
                    },
                )
            };
            let type_name = description.get_property("type").to_js_string();
            *set_local.borrow_mut() = description.clone();
            this.set_property("localDescription", description.clone());
            this.set_property("currentLocalDescription", description);
            this.set_property(
                "signalingState",
                Value::string(if type_name == "offer" {
                    "have-local-offer"
                } else {
                    "stable"
                }),
            );
            dispatch(&this, "signalingstatechange");
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: RTCPeerConnection preserves local signaling state, but \
                     ICE gathering, DTLS/SRTP and network transport require a native WebRTC adapter"
                );
            });
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    let set_remote = Rc::clone(&remote_description);
    connection.set_property(
        "setRemoteDescription",
        realm_rtc_function(move |this, args| {
            if this.get_property("signalingState").to_js_string() == "closed" {
                return w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "RTCPeerConnection is closed",
                )]);
            }
            let description =
                normalize_description(args.first().cloned().unwrap_or(Value::Undefined));
            let type_name = description.get_property("type").to_js_string();
            *set_remote.borrow_mut() = description.clone();
            this.set_property("remoteDescription", description.clone());
            this.set_property("currentRemoteDescription", description);
            this.set_property(
                "signalingState",
                Value::string(if type_name == "offer" {
                    "have-remote-offer"
                } else {
                    "stable"
                }),
            );
            dispatch(&this, "signalingstatechange");
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    let add_candidates = Rc::clone(&candidates);
    connection.set_property(
        "addIceCandidate",
        realm_rtc_function(move |this, args| {
            if this.get_property("signalingState").to_js_string() == "closed" {
                return w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "RTCPeerConnection is closed",
                )]);
            }
            let candidate = args.first().cloned().unwrap_or(Value::Null);
            if !candidate.is_null() {
                add_candidates.borrow_mut().push(
                    if w3cos_core::class::instance_of(&candidate, &class_for("RTCIceCandidate")) {
                        candidate
                    } else {
                        ice_candidate_value(candidate)
                    },
                );
            }
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );

    let add_senders = Rc::clone(&senders);
    connection.set_property(
        "addTrack",
        realm_rtc_function(move |this, args| {
            let track = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::class::instance_of(
                &track,
                &crate::media_devices_web::media_stream_track_class(),
            ) {
                throw("TypeError", "addTrack requires a MediaStreamTrack");
            }
            if add_senders
                .borrow()
                .iter()
                .any(|sender| sender.get_property("track").strict_eq(&track))
            {
                throw("InvalidAccessError", "Track is already attached");
            }
            let sender = rtp_sender_value(track, args.iter().skip(1).cloned().collect());
            add_senders.borrow_mut().push(sender.clone());
            crate::jsdom::queue_microtask_value(realm_rtc_function(move |_, _| {
                dispatch(&this, "negotiationneeded");
                Value::Undefined
            }));
            sender
        }),
    );
    let remove_senders = Rc::clone(&senders);
    connection.set_property(
        "removeTrack",
        realm_rtc_function(move |this, args| {
            let sender = args.first().cloned().unwrap_or(Value::Undefined);
            if !remove_senders
                .borrow()
                .iter()
                .any(|candidate| candidate.strict_eq(&sender))
            {
                throw(
                    "InvalidAccessError",
                    "Sender does not belong to this connection",
                );
            }
            sender.set_property("track", Value::Null);
            dispatch(&this, "negotiationneeded");
            Value::Undefined
        }),
    );

    let add_transceivers = Rc::clone(&transceivers);
    let add_receivers = Rc::clone(&receivers);
    let transceiver_senders = Rc::clone(&senders);
    connection.set_property(
        "addTransceiver",
        realm_rtc_function(move |this, args| {
            let source = args.first().cloned().unwrap_or(Value::Undefined);
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            let (track, kind) = if source.is_string() {
                let kind = source.to_js_string();
                if !matches!(kind.as_str(), "audio" | "video") {
                    throw("TypeError", "Transceiver kind must be audio or video");
                }
                (Value::Null, kind)
            } else if w3cos_core::class::instance_of(
                &source,
                &crate::media_devices_web::media_stream_track_class(),
            ) {
                (source.clone(), source.get_property("kind").to_js_string())
            } else {
                throw(
                    "TypeError",
                    "addTransceiver requires a media kind or MediaStreamTrack",
                );
            };
            let streams = init.get_property("streams").iter().collect();
            let transceiver = transceiver_value(track, kind, init, streams);
            transceiver_senders
                .borrow_mut()
                .push(transceiver.get_property("sender"));
            add_receivers
                .borrow_mut()
                .push(transceiver.get_property("receiver"));
            add_transceivers.borrow_mut().push(transceiver.clone());
            dispatch(&this, "negotiationneeded");
            transceiver
        }),
    );

    for (method, values) in [
        ("getSenders", Rc::clone(&senders)),
        ("getReceivers", Rc::clone(&receivers)),
        ("getTransceivers", Rc::clone(&transceivers)),
    ] {
        connection.set_property(
            method,
            realm_rtc_function(move |_, _| Value::array(values.borrow().clone())),
        );
    }

    let create_channels = Rc::clone(&channels);
    connection.set_property(
        "createDataChannel",
        realm_rtc_function(move |this, args| {
            if this.get_property("signalingState").to_js_string() == "closed" {
                throw("InvalidStateError", "RTCPeerConnection is closed");
            }
            let label = args.first().map(Value::to_js_string).unwrap_or_default();
            let channel = data_channel_value(
                label,
                args.get(1)
                    .cloned()
                    .unwrap_or_else(|| Value::object(HashMap::new())),
            );
            create_channels.borrow_mut().push(channel.clone());
            dispatch(&this, "negotiationneeded");
            channel
        }),
    );

    let add_local_streams = Rc::clone(&local_streams);
    connection.set_property(
        "addStream",
        realm_rtc_function(move |this, args| {
            let stream = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::class::instance_of(
                &stream,
                &crate::media_devices_web::media_stream_class(),
            ) {
                throw("TypeError", "addStream requires a MediaStream");
            }
            if !add_local_streams
                .borrow()
                .iter()
                .any(|candidate| candidate.strict_eq(&stream))
            {
                add_local_streams.borrow_mut().push(stream);
                dispatch(&this, "negotiationneeded");
            }
            Value::Undefined
        }),
    );
    let remove_local_streams = Rc::clone(&local_streams);
    connection.set_property(
        "removeStream",
        realm_rtc_function(move |this, args| {
            let stream = args.first().cloned().unwrap_or(Value::Undefined);
            remove_local_streams
                .borrow_mut()
                .retain(|candidate| !candidate.strict_eq(&stream));
            dispatch(&this, "negotiationneeded");
            Value::Undefined
        }),
    );
    for (method, streams) in [
        ("getLocalStreams", Rc::clone(&local_streams)),
        ("getRemoteStreams", Rc::clone(&remote_streams)),
    ] {
        connection.set_property(
            method,
            realm_rtc_function(move |_, _| Value::array(streams.borrow().clone())),
        );
    }

    connection.set_property(
        "createDTMFSender",
        realm_rtc_function(|_, args| {
            dtmf_sender_value(args.first().cloned().unwrap_or(Value::Null))
        }),
    );
    connection.set_property(
        "getStats",
        realm_rtc_function(|_, _| {
            w3cos_core::promise::resolve(vec![stats_report_value(Vec::new())])
        }),
    );
    let get_configuration = Rc::clone(&configuration);
    connection.set_property(
        "getConfiguration",
        realm_rtc_function(move |_, _| {
            w3cos_core::web::structured_clone(vec![get_configuration.borrow().clone()])
        }),
    );
    connection.set_property(
        "setConfiguration",
        realm_rtc_function(move |_, args| {
            let next = args
                .first()
                .cloned()
                .unwrap_or_else(|| Value::object(HashMap::new()));
            if !next.is_object() {
                throw("TypeError", "setConfiguration requires an object");
            }
            *configuration.borrow_mut() = next;
            Value::Undefined
        }),
    );
    connection.set_property(
        "restartIce",
        realm_rtc_function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: RTCPeerConnection.restartIce cannot gather candidates \
                     without a native ICE transport adapter"
                );
            });
            Value::Undefined
        }),
    );
    let close_channels = Rc::clone(&channels);
    let close_transceivers = Rc::clone(&transceivers);
    connection.set_property(
        "close",
        realm_rtc_function(move |this, _| {
            if this.get_property("signalingState").to_js_string() == "closed" {
                return Value::Undefined;
            }
            this.set_property("signalingState", Value::string("closed"));
            this.set_property("connectionState", Value::string("closed"));
            this.set_property("iceConnectionState", Value::string("closed"));
            for channel in close_channels.borrow().iter() {
                channel.call_method("close", Vec::new());
            }
            for transceiver in close_transceivers.borrow().iter() {
                transceiver.call_method("stop", Vec::new());
            }
            dispatch(&this, "signalingstatechange");
            dispatch(&this, "connectionstatechange");
            dispatch(&this, "iceconnectionstatechange");
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &connection,
        &class_for("RTCPeerConnection").get_property("prototype"),
    );
    register_rtc_value(&connection);
    connection
}

fn script_transform_value(worker: Value, options: Value) -> Value {
    if worker.is_undefined() {
        throw("TypeError", "RTCRtpScriptTransform requires a Worker");
    }
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: RTCRtpScriptTransform retains worker/options identity, but encoded \
             RTP frame delivery requires an isolated WebRTC transform adapter"
        );
    });
    let value = Value::object(HashMap::from([
        ("__w3cos_worker".into(), worker),
        ("__w3cos_options".into(), options),
    ]));
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("RTCRtpScriptTransform").get_property("prototype"),
    );
    register_rtc_value(&value);
    value
}

fn build_class(name: &'static str) -> Value {
    let class = match name {
        "RTCIceCandidate" => realm_rtc_function(|_, args| {
            ice_candidate_value(
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::object(HashMap::new())),
            )
        }),
        "RTCPeerConnection" => realm_rtc_function(|_, args| {
            peer_connection_value(
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::object(HashMap::new())),
            )
        }),
        "RTCRtpScriptTransform" => realm_rtc_function(|_, args| {
            script_transform_value(
                args.first().cloned().unwrap_or(Value::Undefined),
                args.get(1).cloned().unwrap_or(Value::Undefined),
            )
        }),
        "RTCSessionDescription" => realm_rtc_function(|_, args| {
            session_description_value(
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::object(HashMap::new())),
            )
        }),
        "RTCError" => realm_rtc_function(|_, args| {
            rtc_error_value(
                args.first().cloned().unwrap_or(Value::Undefined),
                args.get(1).cloned().unwrap_or(Value::string("")),
            )
        }),
        _ => realm_rtc_function(move |_, _| illegal(name)),
    };
    class.set_property("name", Value::string(name));
    match name {
        "RTCPeerConnection" => {
            class.set_property(
                "generateCertificate",
                realm_rtc_function(|_, _| {
                    static WARNING: Once = Once::new();
                    WARNING.call_once(|| {
                        eprintln!(
                            "[w3cos] warning: RTCPeerConnection.generateCertificate requires a \
                             native WebRTC cryptographic identity provider"
                        );
                    });
                    w3cos_core::promise::reject(vec![error(
                        "NotSupportedError",
                        "WebRTC certificate generation is unavailable",
                    )])
                }),
            );
        }
        "RTCRtpReceiver" | "RTCRtpSender" => {
            class.set_property(
                "getCapabilities",
                realm_rtc_function(|_, args| {
                    capabilities(&args.first().map(Value::to_js_string).unwrap_or_default())
                }),
            );
        }
        _ => {}
    }
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in prototype_members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    if let Some(parent) = parent_name(name) {
        let parent = if parent == "EventTarget" {
            crate::web_events::event_target_class()
        } else if parent == "DOMException" {
            crate::unsupported::dom_exception_class()
        } else {
            class_for(parent)
        };
        w3cos_core::class::set_prototype_of(&prototype, &parent.get_property("prototype"));
    }
    class.set_property("prototype", prototype);
    class
}

fn parent_name(name: &str) -> Option<&'static str> {
    match name {
        "RTCDataChannel" | "RTCDtlsTransport" | "RTCDTMFSender" | "RTCIceTransport"
        | "RTCPeerConnection" | "RTCSctpTransport" => Some("EventTarget"),
        "RTCError" => Some("DOMException"),
        _ => None,
    }
}

fn prototype_members(name: &str) -> &'static [&'static str] {
    match name {
        "RTCCertificate" => &["expires", "getFingerprints"],
        "RTCDTMFSender" => &["canInsertDTMF", "insertDTMF", "ontonechange", "toneBuffer"],
        "RTCDataChannel" => &[
            "binaryType",
            "bufferedAmount",
            "bufferedAmountLowThreshold",
            "close",
            "id",
            "label",
            "maxPacketLifeTime",
            "maxRetransmits",
            "negotiated",
            "onbufferedamountlow",
            "onclose",
            "onclosing",
            "onerror",
            "onmessage",
            "onopen",
            "ordered",
            "protocol",
            "readyState",
            "reliable",
            "send",
        ],
        "RTCDtlsTransport" => &[
            "getRemoteCertificates",
            "iceTransport",
            "onerror",
            "onstatechange",
            "state",
        ],
        "RTCEncodedAudioFrame" => &["data", "getMetadata", "timestamp", "toString"],
        "RTCEncodedVideoFrame" => &["data", "getMetadata", "timestamp", "toString", "type"],
        "RTCError" => &[
            "errorDetail",
            "httpRequestStatusCode",
            "receivedAlert",
            "sctpCauseCode",
            "sdpLineNumber",
            "sentAlert",
        ],
        "RTCIceCandidate" => &[
            "address",
            "candidate",
            "component",
            "foundation",
            "port",
            "priority",
            "protocol",
            "relatedAddress",
            "relatedPort",
            "relayProtocol",
            "sdpMLineIndex",
            "sdpMid",
            "tcpType",
            "toJSON",
            "type",
            "url",
            "usernameFragment",
        ],
        "RTCIceTransport" => &[
            "gatheringState",
            "getLocalCandidates",
            "getLocalParameters",
            "getRemoteCandidates",
            "getRemoteParameters",
            "getSelectedCandidatePair",
            "ongatheringstatechange",
            "onselectedcandidatepairchange",
            "onstatechange",
            "role",
            "state",
        ],
        "RTCPeerConnection" => &[
            "addIceCandidate",
            "addStream",
            "addTrack",
            "addTransceiver",
            "canTrickleIceCandidates",
            "close",
            "connectionState",
            "createAnswer",
            "createDTMFSender",
            "createDataChannel",
            "createOffer",
            "currentLocalDescription",
            "currentRemoteDescription",
            "getConfiguration",
            "getLocalStreams",
            "getReceivers",
            "getRemoteStreams",
            "getSenders",
            "getStats",
            "getTransceivers",
            "iceConnectionState",
            "iceGatheringState",
            "localDescription",
            "onaddstream",
            "onconnectionstatechange",
            "ondatachannel",
            "onicecandidate",
            "onicecandidateerror",
            "oniceconnectionstatechange",
            "onicegatheringstatechange",
            "onnegotiationneeded",
            "onremovestream",
            "onsignalingstatechange",
            "ontrack",
            "pendingLocalDescription",
            "pendingRemoteDescription",
            "remoteDescription",
            "removeStream",
            "removeTrack",
            "restartIce",
            "sctp",
            "setConfiguration",
            "setLocalDescription",
            "setRemoteDescription",
            "signalingState",
        ],
        "RTCRtpReceiver" => &[
            "createEncodedStreams",
            "getContributingSources",
            "getParameters",
            "getStats",
            "getSynchronizationSources",
            "jitterBufferTarget",
            "playoutDelayHint",
            "rtcpTransport",
            "track",
            "transform",
            "transport",
        ],
        "RTCRtpSender" => &[
            "createEncodedStreams",
            "dtmf",
            "getParameters",
            "getStats",
            "replaceTrack",
            "rtcpTransport",
            "setParameters",
            "setStreams",
            "track",
            "transform",
            "transport",
        ],
        "RTCRtpTransceiver" => &[
            "currentDirection",
            "direction",
            "getHeaderExtensionsToNegotiate",
            "getNegotiatedHeaderExtensions",
            "mid",
            "receiver",
            "sender",
            "setCodecPreferences",
            "setHeaderExtensionsToNegotiate",
            "stop",
            "stopped",
        ],
        "RTCSctpTransport" => &[
            "maxChannels",
            "maxMessageSize",
            "onstatechange",
            "state",
            "transport",
        ],
        "RTCSessionDescription" => &["sdp", "toJSON", "type"],
        "RTCStatsReport" => &["entries", "forEach", "get", "has", "keys", "size", "values"],
        _ => &[],
    }
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

pub fn classes() -> Vec<(&'static str, Value)> {
    NAMES.iter().map(|name| (*name, class_for(name))).collect()
}

pub fn reset() {
    VALUES.with(|values| {
        for value in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            for name in NAMES {
                for member in prototype_members(name) {
                    value.set_property(member, Value::Undefined);
                }
            }
            for reference in [
                "__w3cos_codec_preferences",
                "__w3cos_header_extensions",
                "__w3cos_options",
                "__w3cos_parameters",
                "__w3cos_streams",
                "__w3cos_symbol_iterator",
                "__w3cos_track",
                "__w3cos_worker",
                "readable",
                "writable",
            ] {
                value.set_property(reference, Value::Undefined);
            }
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        class.set_property("generateCertificate", Value::Undefined);
        class.set_property("getCapabilities", Value::Undefined);
        disconnect_realm_class(class);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signaling_descriptions_and_close_state_are_live() {
        let connection = peer_connection_value(Value::object(HashMap::new()));
        let offer = Rc::new(RefCell::new(Value::Undefined));
        let offer_for_callback = Rc::clone(&offer);
        connection
            .call_method("createOffer", Vec::new())
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *offer_for_callback.borrow_mut() =
                        args.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        connection.call_method("setLocalDescription", vec![offer.borrow().clone()]);
        assert_eq!(
            connection.get_property("signalingState").to_js_string(),
            "have-local-offer"
        );
        assert!(
            connection
                .get_property("localDescription")
                .get_property("sdp")
                .to_js_string()
                .starts_with("v=0")
        );
        connection.call_method("close", Vec::new());
        assert_eq!(
            connection.get_property("connectionState").to_js_string(),
            "closed"
        );
    }

    #[test]
    fn tracks_create_sender_receiver_and_transceiver_relationships() {
        let connection = peer_connection_value(Value::object(HashMap::new()));
        let track = crate::media_devices_web::track_value("audio", "microphone");
        let sender = connection.call_method("addTrack", vec![track.clone()]);
        assert!(sender.get_property("track").strict_eq(&track));
        assert!(w3cos_core::class::instance_of(
            &sender.get_property("dtmf"),
            &class_for("RTCDTMFSender")
        ));
        let transceiver = connection.call_method("addTransceiver", vec![Value::string("video")]);
        assert_eq!(
            transceiver
                .get_property("receiver")
                .get_property("track")
                .get_property("kind")
                .to_js_string(),
            "video"
        );
        assert_eq!(
            connection
                .call_method("getSenders", Vec::new())
                .get_property("length")
                .to_number(),
            2.0
        );
    }

    #[test]
    fn data_channel_retains_options_and_closes_without_fake_transport() {
        let connection = peer_connection_value(Value::object(HashMap::new()));
        let channel = connection.call_method(
            "createDataChannel",
            vec![
                Value::string("events"),
                Value::object(HashMap::from([
                    ("ordered".into(), Value::Bool(false)),
                    ("protocol".into(), Value::string("json")),
                ])),
            ],
        );
        assert_eq!(channel.get_property("label").to_js_string(), "events");
        assert!(!channel.get_property("ordered").to_bool());
        assert_eq!(
            channel.get_property("readyState").to_js_string(),
            "connecting"
        );
        channel.call_method("close", Vec::new());
        assert_eq!(channel.get_property("readyState").to_js_string(), "closed");
    }

    #[test]
    fn rtc_classes_resources_callbacks_and_microtasks_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_peer_class = class_for("RTCPeerConnection");
        let old_channel_class = class_for("RTCDataChannel");
        let old_description_class = class_for("RTCSessionDescription");
        assert!(old_peer_class.strict_eq(&class_for("RTCPeerConnection")));
        assert!(old_channel_class.strict_eq(&class_for("RTCDataChannel")));

        let connection =
            w3cos_core::class::construct(&old_peer_class, vec![Value::object(HashMap::new())]);
        let create_offer = connection.get_property("createOffer");
        let close_connection = connection.get_property("close");
        let callback_marker = Rc::new(());
        let callback_marker_weak = Rc::downgrade(&callback_marker);
        connection.set_property(
            "onnegotiationneeded",
            Value::function(move |_, _| {
                let _ = &callback_marker;
                Value::Undefined
            }),
        );
        let track = crate::media_devices_web::track_value("audio", "old microphone");
        connection.call_method("addTrack", vec![track]);
        let channel = connection.call_method("createDataChannel", vec![Value::string("old")]);
        let close_channel = channel.get_property("close");
        let description = w3cos_core::class::construct(
            &old_description_class,
            vec![Value::object(HashMap::from([
                ("type".into(), Value::string("offer")),
                ("sdp".into(), Value::string("v=0\r\n")),
            ]))],
        );
        let to_json = description.get_property("toJSON");

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_peer_class.strict_eq(&class_for("RTCPeerConnection")));
        assert!(!old_channel_class.strict_eq(&class_for("RTCDataChannel")));
        assert!(!old_description_class.strict_eq(&class_for("RTCSessionDescription")));
        for class in [&old_peer_class, &old_channel_class, &old_description_class] {
            assert!(class.get_property("prototype").is_undefined());
            assert!(class.call(Value::Undefined, vec![]).is_undefined());
        }
        assert!(create_offer.call(connection.clone(), vec![]).is_undefined());
        assert!(
            close_connection
                .call(connection.clone(), vec![])
                .is_undefined()
        );
        assert!(connection.get_property("signalingState").is_undefined());
        assert!(connection.get_property("createOffer").is_undefined());
        assert!(close_channel.call(channel.clone(), vec![]).is_undefined());
        assert!(channel.get_property("readyState").is_undefined());
        assert!(channel.get_property("close").is_undefined());
        assert!(to_json.call(description, vec![]).is_undefined());
        assert!(
            connection
                .get_property("onnegotiationneeded")
                .is_undefined()
        );
        assert!(callback_marker_weak.upgrade().is_none());
        crate::jsdom::drain_microtasks();
    }
}
