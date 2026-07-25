//! Browser-shaped Web Speech recognition facade.
//!
//! Recognition work is delegated to [`crate::speech`]. This module owns the
//! JavaScript object identity, result arrays, and lifecycle event delivery.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static SPEECH_RECOGNITION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SPEECH_GRAMMAR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SPEECH_GRAMMAR_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SPEECH_RECOGNITION_PHRASE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ACTIVE: RefCell<Option<ActiveRecognition>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct ActiveRecognition {
    value: Value,
    transcript_signal: usize,
    final_signal: usize,
    confidence_signal: usize,
    status_signal: usize,
    previous_status: i64,
    previous_transcript: String,
    previous_final: i64,
    previous_confidence: i64,
}

fn event(event_type: &str) -> Value {
    w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_type)],
    )
}

fn dispatch(value: &Value, event_type: &str) {
    value.call_method("dispatchEvent", vec![event(event_type)]);
}

fn dispatch_started(value: &Value) {
    for event_type in ["start", "audiostart", "soundstart", "speechstart"] {
        dispatch(value, event_type);
    }
}

fn dispatch_ended(value: &Value) {
    for event_type in ["speechend", "soundend", "audioend", "end"] {
        dispatch(value, event_type);
    }
}

fn result_event(transcript: &str, is_final: bool, confidence_percent: i64) -> Value {
    let alternative = Value::object(HashMap::from([
        ("transcript".to_string(), Value::string(transcript)),
        (
            "confidence".to_string(),
            Value::Number(confidence_percent.clamp(0, 100) as f64 / 100.0),
        ),
    ]));
    let result = Value::object(HashMap::from([
        ("0".to_string(), alternative),
        ("length".to_string(), Value::Number(1.0)),
        ("isFinal".to_string(), Value::Bool(is_final)),
    ]));
    let results = Value::object(HashMap::from([
        ("0".to_string(), result),
        ("length".to_string(), Value::Number(1.0)),
    ]));
    let event = event("result");
    event.set_property("resultIndex", Value::Number(0.0));
    event.set_property("results", results);
    event
}

fn error_name(status: i64) -> &'static str {
    match status {
        -2 => "not-allowed",
        -3 => "language-not-supported",
        -4 => "service-not-allowed",
        -5 => "audio-capture",
        -6 => "no-speech",
        _ => "not-supported",
    }
}

fn start_recognition(value: Value) {
    ACTIVE.with(|active| {
        if active.borrow().is_some() {
            crate::speech::stop();
        }
        let transcript_signal = crate::state::create_text_signal("");
        let final_signal = crate::state::create_signal(0);
        let confidence_signal = crate::state::create_signal(0);
        let status_signal = crate::state::create_signal(0);
        crate::speech::start(crate::speech::SpeechRecognitionBinding {
            transcript_signal,
            final_signal,
            confidence_signal,
            status_signal,
            options: crate::speech::SpeechRecognitionOptions {
                lang: value.get_property("lang").to_js_string(),
                process_locally: value.get_property("processLocally").to_bool(),
                continuous: value.get_property("continuous").to_bool(),
                interim_results: value.get_property("interimResults").to_bool(),
            },
        });
        *active.borrow_mut() = Some(ActiveRecognition {
            value,
            transcript_signal,
            final_signal,
            confidence_signal,
            status_signal,
            previous_status: 1,
            previous_transcript: String::new(),
            previous_final: 0,
            previous_confidence: 0,
        });
    });
}

fn recognition_value() -> Value {
    let value = Value::object(HashMap::from([
        ("lang".to_string(), Value::string("en-US")),
        ("continuous".to_string(), Value::Bool(false)),
        ("interimResults".to_string(), Value::Bool(false)),
        ("maxAlternatives".to_string(), Value::Number(1.0)),
        ("processLocally".to_string(), Value::Bool(false)),
        ("grammars".to_string(), speech_grammar_list_value()),
        ("phrases".to_string(), Value::array(Vec::new())),
    ]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    for name in [
        "start",
        "end",
        "error",
        "result",
        "nomatch",
        "audiostart",
        "audioend",
        "soundstart",
        "soundend",
        "speechstart",
        "speechend",
    ] {
        value.set_property(&format!("on{name}"), Value::Null);
    }
    value.set_property(
        "start",
        Value::function(|this, _| {
            start_recognition(this);
            Value::Undefined
        }),
    );
    value.set_property(
        "stop",
        Value::function(|_, _| {
            crate::speech::stop();
            Value::Undefined
        }),
    );
    value.set_property(
        "abort",
        Value::function(|_, _| {
            crate::speech::stop();
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &speech_recognition_class().get_property("prototype"),
    );
    value
}

pub fn speech_grammar_class() -> Value {
    SPEECH_GRAMMAR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            this.set_property(
                "src",
                Value::string(&args.first().map(Value::to_js_string).unwrap_or_default()),
            );
            this.set_property(
                "weight",
                Value::Number(args.get(1).map(Value::to_number).unwrap_or(1.0)),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("SpeechGrammar"));
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), class.clone()),
            ("src".into(), Value::Undefined),
            ("weight".into(), Value::Undefined),
        ]));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn refresh_grammar_list(list: &Value, grammars: &[Value], old_length: usize) {
    for index in 0..old_length.max(grammars.len()) {
        list.set_property(
            &index.to_string(),
            grammars.get(index).cloned().unwrap_or(Value::Undefined),
        );
    }
    list.set_property("length", Value::Number(grammars.len() as f64));
}

fn speech_grammar_list_value() -> Value {
    let grammars = std::rc::Rc::new(RefCell::new(Vec::<Value>::new()));
    let list = Value::object(HashMap::from([("length".into(), Value::Number(0.0))]));
    let item_grammars = std::rc::Rc::clone(&grammars);
    list.set_property(
        "item",
        Value::function(move |_, args| {
            item_grammars
                .borrow()
                .get(args.first().map(Value::to_u32).unwrap_or_default() as usize)
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    for (method, uri) in [("addFromString", false), ("addFromUri", true)] {
        let add_grammars = std::rc::Rc::clone(&grammars);
        let add_list = list.clone();
        list.set_property(
            method,
            Value::function(move |_, args| {
                let source = args.first().map(Value::to_js_string).unwrap_or_default();
                if source.is_empty() {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string("Speech grammar source must not be empty")],
                    ));
                }
                if uri {
                    eprintln!(
                        "[w3cos] warning: SpeechGrammarList retains grammar URIs, but fetching \
                         and compiling remote grammars requires a speech adapter"
                    );
                }
                let grammar = w3cos_core::class::construct(
                    &speech_grammar_class(),
                    vec![
                        Value::string(&source),
                        Value::Number(args.get(1).map(Value::to_number).unwrap_or(1.0)),
                    ],
                );
                let old_length = add_grammars.borrow().len();
                add_grammars.borrow_mut().push(grammar);
                refresh_grammar_list(&add_list, &add_grammars.borrow(), old_length);
                Value::Undefined
            }),
        );
    }
    w3cos_core::class::set_prototype_of(
        &list,
        &speech_grammar_list_class().get_property("prototype"),
    );
    list
}

pub fn speech_grammar_list_class() -> Value {
    SPEECH_GRAMMAR_LIST_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| speech_grammar_list_value());
        class.set_property("name", Value::string("SpeechGrammarList"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ["addFromString", "addFromUri", "item", "length"] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn speech_recognition_phrase_class() -> Value {
    SPEECH_RECOGNITION_PHRASE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let phrase = args.first().map(Value::to_js_string).unwrap_or_default();
            let boost = args.get(1).map(Value::to_number).unwrap_or(1.0);
            if phrase.is_empty() || !boost.is_finite() || !(0.0..=10.0).contains(&boost) {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string(
                        "SpeechRecognitionPhrase requires text and a boost from 0 to 10",
                    )],
                ));
            }
            this.set_property("phrase", Value::string(&phrase));
            this.set_property("boost", Value::Number(boost));
            Value::Undefined
        });
        class.set_property("name", Value::string("SpeechRecognitionPhrase"));
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), class.clone()),
            ("boost".into(), Value::Undefined),
            ("phrase".into(), Value::Undefined),
        ]));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn speech_recognition_class() -> Value {
    SPEECH_RECOGNITION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| recognition_value());
        class.set_property("name", Value::string("SpeechRecognition"));
        class.set_property(
            "available",
            Value::function(|_, _| {
                eprintln!(
                    "[w3cos] warning: SpeechRecognition.available reports the local runtime \
                     adapter state; language-pack discovery is not available"
                );
                w3cos_core::promise::resolve(vec![Value::string("unavailable")])
            }),
        );
        class.set_property(
            "install",
            Value::function(|_, _| {
                eprintln!(
                    "[w3cos] warning: SpeechRecognition.install cannot install browser language \
                     packs; configure a native speech adapter instead"
                );
                w3cos_core::promise::resolve(vec![Value::Bool(false)])
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "abort",
            "continuous",
            "grammars",
            "interimResults",
            "lang",
            "maxAlternatives",
            "onaudioend",
            "onaudiostart",
            "onend",
            "onerror",
            "onnomatch",
            "onresult",
            "onsoundend",
            "onsoundstart",
            "onspeechend",
            "onspeechstart",
            "onstart",
            "phrases",
            "processLocally",
            "quality",
            "start",
            "stop",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

/// Poll the native engine and dispatch browser lifecycle events.
pub fn poll_js_events() -> usize {
    let _ = crate::speech::poll();
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(current) = active.as_mut() else {
            return 0;
        };
        let status = crate::state::get_signal(current.status_signal);
        let transcript = crate::state::get_signal_text(current.transcript_signal);
        let is_final = crate::state::get_signal(current.final_signal);
        let confidence = crate::state::get_signal(current.confidence_signal);
        let mut dispatched = 0;

        if status == 2 && current.previous_status != 2 {
            dispatch_started(&current.value);
            dispatched += 4;
        }
        if !transcript.is_empty()
            && (transcript != current.previous_transcript
                || is_final != current.previous_final
                || confidence != current.previous_confidence)
            && status >= 0
        {
            current.value.call_method(
                "dispatchEvent",
                vec![result_event(&transcript, is_final != 0, confidence)],
            );
            dispatched += 1;
        }
        if status < 0 {
            let event = event("error");
            event.set_property("error", Value::string(error_name(status)));
            event.set_property("message", Value::string(&transcript));
            current.value.call_method("dispatchEvent", vec![event]);
            dispatch_ended(&current.value);
            dispatched += 5;
            *active = None;
            return dispatched;
        }
        if status == 3 {
            dispatch_ended(&current.value);
            dispatched += 4;
            *active = None;
            return dispatched;
        }

        current.previous_status = status;
        current.previous_transcript = transcript;
        current.previous_final = is_final;
        current.previous_confidence = confidence;
        dispatched
    })
}

pub fn reset() {
    crate::speech::stop();
    ACTIVE.with(|active| *active.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[cfg(not(target_os = "ios"))]
    #[test]
    fn unsupported_host_dispatches_explicit_error_and_end() {
        reset();
        let recognition = w3cos_core::class::construct(&speech_recognition_class(), Vec::new());
        let events = Rc::new(RefCell::new(Vec::new()));
        for name in ["error", "end"] {
            let events = events.clone();
            recognition.set_property(
                &format!("on{name}"),
                Value::function(move |_, args| {
                    let event = args.first().cloned().unwrap_or_default();
                    events.borrow_mut().push((
                        event.get_property("type").to_js_string(),
                        event.get_property("error").to_js_string(),
                    ));
                    Value::Undefined
                }),
            );
        }
        recognition.call_method("start", vec![]);
        assert_eq!(poll_js_events(), 5);
        assert_eq!(
            events.borrow().as_slice(),
            [
                ("error".to_string(), "service-not-allowed".to_string()),
                ("end".to_string(), "undefined".to_string()),
            ]
        );
    }

    #[test]
    fn grammar_lists_and_contextual_phrases_are_stateful() {
        let list = w3cos_core::class::construct(&speech_grammar_list_class(), Vec::new());
        list.call_method(
            "addFromString",
            vec![
                Value::string("#JSGF V1.0; grammar colors;"),
                Value::Number(0.5),
            ],
        );
        assert_eq!(list.get_property("length").to_u32(), 1);
        assert_eq!(
            list.call_method("item", vec![Value::Number(0.0)])
                .get_property("weight")
                .to_number(),
            0.5
        );
        let phrase = w3cos_core::class::construct(
            &speech_recognition_phrase_class(),
            vec![Value::string("w3cos"), Value::Number(4.0)],
        );
        assert_eq!(phrase.get_property("phrase").to_js_string(), "w3cos");
        assert_eq!(phrase.get_property("boost").to_number(), 4.0);

        let recognition = w3cos_core::class::construct(&speech_recognition_class(), Vec::new());
        assert!(w3cos_core::class::instance_of(
            &recognition.get_property("grammars"),
            &speech_grammar_list_class()
        ));
    }
}
