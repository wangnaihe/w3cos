//! Browser-shaped Web Speech synthesis queue.
//!
//! The runtime preserves queue state and utterance events even when a target
//! has no native text-to-speech adapter. Audio production is an explicit
//! warning-backed host boundary.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static SYNTHESIS_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static UTTERANCE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static VOICE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SYNTHESIS_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static QUEUED: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static CURRENT: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PAUSED: Cell<bool> = const { Cell::new(false) };
}

fn illegal_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    members: &'static [&'static str],
    event_target: bool,
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(&format!("Illegal constructor: {name}"))],
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in members {
            prototype.set_property(member, Value::Undefined);
        }
        if event_target {
            w3cos_core::class::set_prototype_of(
                &prototype,
                &crate::web_events::event_target_class().get_property("prototype"),
            );
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn speech_synthesis_class() -> Value {
    illegal_class(
        &SYNTHESIS_CLASS,
        "SpeechSynthesis",
        &[
            "cancel",
            "getVoices",
            "onvoiceschanged",
            "pause",
            "paused",
            "pending",
            "resume",
            "speak",
            "speaking",
        ],
        true,
    )
}

pub fn speech_synthesis_voice_class() -> Value {
    illegal_class(
        &VOICE_CLASS,
        "SpeechSynthesisVoice",
        &["default", "lang", "localService", "name", "voiceURI"],
        false,
    )
}

pub fn speech_synthesis_utterance_class() -> Value {
    UTTERANCE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            this.set_property(
                "text",
                Value::string(&args.first().map(Value::to_js_string).unwrap_or_default()),
            );
            this.set_property("lang", Value::string(""));
            this.set_property("pitch", Value::Number(1.0));
            this.set_property("rate", Value::Number(1.0));
            this.set_property("volume", Value::Number(1.0));
            this.set_property("voice", Value::Null);
            for handler in [
                "onboundary",
                "onend",
                "onerror",
                "onmark",
                "onpause",
                "onresume",
                "onstart",
            ] {
                this.set_property(handler, Value::Null);
            }
            Value::Undefined
        });
        class.set_property("name", Value::string("SpeechSynthesisUtterance"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "lang",
            "onboundary",
            "onend",
            "onerror",
            "onmark",
            "onpause",
            "onresume",
            "onstart",
            "pitch",
            "rate",
            "text",
            "voice",
            "volume",
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

fn synthesis_event(utterance: &Value, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("SpeechSynthesisEvent"),
        vec![
            Value::string(event_type),
            Value::object(HashMap::from([
                ("utterance".into(), utterance.clone()),
                ("charIndex".into(), Value::Number(0.0)),
                ("charLength".into(), Value::Number(0.0)),
                ("elapsedTime".into(), Value::Number(0.0)),
                ("name".into(), Value::string("")),
            ])),
        ],
    );
    utterance.call_method("dispatchEvent", vec![event]);
}

fn schedule_next() {
    if PAUSED.with(Cell::get) || CURRENT.with(|current| current.borrow().is_some()) {
        return;
    }
    let next = QUEUED.with(|queued| {
        let mut queued = queued.borrow_mut();
        if queued.is_empty() {
            None
        } else {
            Some(queued.remove(0))
        }
    });
    let Some(utterance) = next else {
        return;
    };
    CURRENT.with(|current| *current.borrow_mut() = Some(utterance.clone()));
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        let still_current = CURRENT.with(|current| current.borrow().as_ref() == Some(&utterance));
        if !still_current {
            return Value::Undefined;
        }
        if PAUSED.with(Cell::get) {
            CURRENT.with(|current| *current.borrow_mut() = None);
            QUEUED.with(|queued| queued.borrow_mut().insert(0, utterance.clone()));
            return Value::Undefined;
        }
        synthesis_event(&utterance, "start");
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: speechSynthesis preserves utterance queue state and events; \
                 audible text-to-speech output requires a native synthesis adapter"
            );
        });
        synthesis_event(&utterance, "end");
        CURRENT.with(|current| *current.borrow_mut() = None);
        schedule_next();
        Value::Undefined
    }));
}

pub fn speech_synthesis_value() -> Value {
    SYNTHESIS_VALUE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::from([("onvoiceschanged".into(), Value::Null)]));
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        value.set_property(
            "__w3cos_getter_pending",
            Value::function(|_, _| Value::Bool(QUEUED.with(|queued| !queued.borrow().is_empty()))),
        );
        value.set_property(
            "__w3cos_getter_speaking",
            Value::function(|_, _| Value::Bool(CURRENT.with(|current| current.borrow().is_some()))),
        );
        value.set_property(
            "__w3cos_getter_paused",
            Value::function(|_, _| Value::Bool(PAUSED.with(Cell::get))),
        );
        value.set_property(
            "getVoices",
            Value::function(|_, _| {
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: speechSynthesis.getVoices() returns an empty compatible \
                         list until a native synthesis adapter provides voices"
                    );
                });
                Value::array(vec![])
            }),
        );
        value.set_property(
            "speak",
            Value::function(|_, args| {
                let utterance = args.first().cloned().unwrap_or_default();
                if !w3cos_core::class::instance_of(&utterance, &speech_synthesis_utterance_class())
                {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string(
                            "speechSynthesis.speak requires a SpeechSynthesisUtterance",
                        )],
                    ));
                }
                QUEUED.with(|queued| queued.borrow_mut().push(utterance));
                schedule_next();
                Value::Undefined
            }),
        );
        value.set_property(
            "pause",
            Value::function(|_, _| {
                PAUSED.with(|paused| paused.set(true));
                if let Some(current) = CURRENT.with(|current| current.borrow().clone()) {
                    synthesis_event(&current, "pause");
                }
                Value::Undefined
            }),
        );
        value.set_property(
            "resume",
            Value::function(|_, _| {
                let was_paused = PAUSED.with(|paused| paused.replace(false));
                if was_paused {
                    if let Some(current) = CURRENT.with(|current| current.borrow().clone()) {
                        synthesis_event(&current, "resume");
                    }
                    schedule_next();
                }
                Value::Undefined
            }),
        );
        value.set_property(
            "cancel",
            Value::function(|_, _| {
                QUEUED.with(|queued| queued.borrow_mut().clear());
                CURRENT.with(|current| *current.borrow_mut() = None);
                PAUSED.with(|paused| paused.set(false));
                Value::Undefined
            }),
        );
        w3cos_core::class::set_prototype_of(
            &value,
            &speech_synthesis_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn reset() {
    QUEUED.with(|queued| queued.borrow_mut().clear());
    CURRENT.with(|current| *current.borrow_mut() = None);
    PAUSED.with(|paused| paused.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn utterances_are_queued_and_emit_start_end_events() {
        reset();
        let synthesis = speech_synthesis_value();
        let utterance = w3cos_core::class::construct(
            &speech_synthesis_utterance_class(),
            vec![Value::string("hello")],
        );
        let events = Rc::new(RefCell::new(Vec::new()));
        for event_type in ["start", "end"] {
            let events_for_listener = Rc::clone(&events);
            utterance.call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, args| {
                        events_for_listener
                            .borrow_mut()
                            .push(args[0].get_property("type").to_js_string());
                        Value::Undefined
                    }),
                ],
            );
        }
        synthesis.call_method("speak", vec![utterance.clone()]);
        assert!(synthesis.get_property("speaking").to_bool());
        crate::jsdom::drain_microtasks();
        assert_eq!(&*events.borrow(), &["start", "end"]);
        assert!(!synthesis.get_property("speaking").to_bool());
        assert!(w3cos_core::class::instance_of(
            &utterance,
            &crate::web_events::event_target_class()
        ));
    }
}
