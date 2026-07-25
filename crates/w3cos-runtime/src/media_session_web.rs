//! Media Session API state and host-action bridge.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionState {
    pub duration: f64,
    pub playback_rate: f64,
    pub position: f64,
}

struct MediaSessionState {
    metadata: Value,
    playback_state: String,
    handlers: HashMap<String, Value>,
    position: Option<PositionState>,
}

impl Default for MediaSessionState {
    fn default() -> Self {
        Self {
            metadata: Value::Null,
            playback_state: "none".into(),
            handlers: HashMap::new(),
            position: None,
        }
    }
}

thread_local! {
    static MEDIA_METADATA_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_SESSION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CHAPTER_INFORMATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_SESSION: RefCell<Option<Value>> = const { RefCell::new(None) };
    static STATE: Rc<RefCell<MediaSessionState>> =
        Rc::new(RefCell::new(MediaSessionState::default()));
}

pub fn chapter_information_class() -> Value {
    CHAPTER_INFORMATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            let start_time = init.get_property("startTime").to_number();
            if !start_time.is_finite() || start_time < 0.0 {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "ChapterInformation startTime must be a non-negative finite number",
                ));
            }
            this.set_property("startTime", Value::Number(start_time));
            this.set_property(
                "title",
                Value::string(&init.get_property("title").to_js_string()),
            );
            let artwork = init.get_property("artwork");
            this.set_property(
                "artwork",
                if artwork.is_undefined() {
                    Value::array(Vec::new())
                } else {
                    Value::array(artwork.iter().collect())
                },
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("ChapterInformation"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ["artwork", "startTime", "title"] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn chapter_information_value(init: Value) -> Value {
    w3cos_core::class::construct(&chapter_information_class(), vec![init])
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn warn_host_adapter() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: Media Session state and action handlers are active in the runtime; \
             publishing metadata and receiving media-key actions require a platform media-center adapter"
        );
    });
}

fn supported_action(action: &str) -> bool {
    matches!(
        action,
        "play"
            | "pause"
            | "seekbackward"
            | "seekforward"
            | "previoustrack"
            | "nexttrack"
            | "skipad"
            | "stop"
            | "seekto"
            | "togglemicrophone"
            | "togglecamera"
            | "hangup"
    )
}

pub fn media_metadata_class() -> Value {
    MEDIA_METADATA_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            for name in ["title", "artist", "album"] {
                let value = init.get_property(name);
                this.set_property(
                    name,
                    if value.is_undefined() {
                        Value::string("")
                    } else {
                        Value::string(&value.to_js_string())
                    },
                );
            }
            let artwork = init.get_property("artwork");
            this.set_property(
                "artwork",
                if artwork.is_undefined() {
                    Value::array(vec![])
                } else {
                    Value::array(artwork.iter().collect())
                },
            );
            let chapters = init.get_property("chapterInfo");
            this.set_property(
                "chapterInfo",
                if chapters.is_undefined() {
                    Value::array(Vec::new())
                } else {
                    Value::array(chapters.iter().map(chapter_information_value).collect())
                },
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("MediaMetadata"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["album", "artist", "artwork", "chapterInfo", "title"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn media_session_class() -> Value {
    MEDIA_SESSION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: MediaSession"))
        });
        class.set_property("name", Value::string("MediaSession"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "metadata",
            "playbackState",
            "setActionHandler",
            "setCameraActive",
            "setMicrophoneActive",
            "setPositionState",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn set_position_state(value: Value) {
    if value.is_undefined() {
        STATE.with(|state| state.borrow_mut().position = None);
        return;
    }
    if !value.is_object() {
        w3cos_core::throw_value(error(
            "TypeError",
            "setPositionState requires a position state object",
        ));
    }
    let duration = value.get_property("duration").to_number();
    let playback_rate_value = value.get_property("playbackRate");
    let playback_rate = if playback_rate_value.is_undefined() {
        1.0
    } else {
        playback_rate_value.to_number()
    };
    let position_value = value.get_property("position");
    let position = if position_value.is_undefined() {
        0.0
    } else {
        position_value.to_number()
    };
    if !duration.is_finite() || duration <= 0.0 {
        w3cos_core::throw_value(error(
            "TypeError",
            "position state duration must be finite and greater than zero",
        ));
    }
    if !playback_rate.is_finite() || playback_rate <= 0.0 {
        w3cos_core::throw_value(error(
            "TypeError",
            "position state playbackRate must be finite and greater than zero",
        ));
    }
    if !position.is_finite() || position < 0.0 || position > duration {
        w3cos_core::throw_value(error(
            "TypeError",
            "position state position must be between zero and duration",
        ));
    }
    STATE.with(|state| {
        state.borrow_mut().position = Some(PositionState {
            duration,
            playback_rate,
            position,
        });
    });
    warn_host_adapter();
}

pub fn media_session_value() -> Value {
    MEDIA_SESSION.with(|slot| {
        if let Some(session) = slot.borrow().clone() {
            return session;
        }
        let session = Value::object(HashMap::new());
        w3cos_core::class::set_prototype_of(
            &session,
            &media_session_class().get_property("prototype"),
        );
        session.set_property(
            "__w3cos_getter_metadata",
            Value::function(|_, _| STATE.with(|state| state.borrow().metadata.clone())),
        );
        session.set_property(
            "__w3cos_setter_metadata",
            Value::function(|_, args| {
                let metadata = args.first().cloned().unwrap_or(Value::Undefined);
                if !metadata.is_null()
                    && !w3cos_core::class::instance_of(&metadata, &media_metadata_class())
                {
                    w3cos_core::throw_value(error(
                        "TypeError",
                        "mediaSession.metadata must be MediaMetadata or null",
                    ));
                }
                STATE.with(|state| state.borrow_mut().metadata = metadata);
                warn_host_adapter();
                Value::Undefined
            }),
        );
        session.set_property(
            "__w3cos_getter_playbackState",
            Value::function(|_, _| {
                STATE.with(|state| Value::string(&state.borrow().playback_state))
            }),
        );
        session.set_property(
            "__w3cos_setter_playbackState",
            Value::function(|_, args| {
                let playback_state = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                if !matches!(playback_state.as_str(), "none" | "paused" | "playing") {
                    w3cos_core::throw_value(error(
                        "TypeError",
                        "playbackState must be none, paused, or playing",
                    ));
                }
                STATE.with(|state| state.borrow_mut().playback_state = playback_state);
                warn_host_adapter();
                Value::Undefined
            }),
        );
        session.set_property(
            "setActionHandler",
            Value::function(|_, args| {
                let action = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                if !supported_action(&action) {
                    w3cos_core::throw_value(error(
                        "NotSupportedError",
                        &format!("unsupported media session action: {action}"),
                    ));
                }
                let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !handler.is_null() && !handler.is_function() {
                    w3cos_core::throw_value(error(
                        "TypeError",
                        "media session action handler must be a function or null",
                    ));
                }
                STATE.with(|state| {
                    if handler.is_null() {
                        state.borrow_mut().handlers.remove(&action);
                    } else {
                        state.borrow_mut().handlers.insert(action, handler);
                        warn_host_adapter();
                    }
                });
                Value::Undefined
            }),
        );
        session.set_property(
            "setPositionState",
            Value::function(|_, args| {
                set_position_state(args.first().cloned().unwrap_or(Value::Undefined));
                Value::Undefined
            }),
        );
        *slot.borrow_mut() = Some(session.clone());
        session
    })
}

/// Deliver an action from a future platform media-center adapter.
pub fn dispatch_action(action: &str, details: Value) -> bool {
    let handler = STATE.with(|state| state.borrow().handlers.get(action).cloned());
    let Some(handler) = handler else {
        return false;
    };
    let event = if details.is_object() {
        details
    } else {
        Value::object(HashMap::new())
    };
    event.set_property("action", Value::string(action));
    handler.call(Value::Undefined, vec![event]);
    true
}

pub fn current_position_state() -> Option<PositionState> {
    STATE.with(|state| state.borrow().position)
}

pub fn reset() {
    STATE.with(|state| *state.borrow_mut() = MediaSessionState::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn metadata_state_actions_and_position_are_browser_shaped() {
        reset();
        let metadata = w3cos_core::class::construct(
            &media_metadata_class(),
            vec![Value::object(HashMap::from([
                ("title".into(), Value::string("Aligned")),
                ("artist".into(), Value::string("W3COS")),
                (
                    "artwork".into(),
                    Value::array(vec![Value::object(HashMap::from([(
                        "src".into(),
                        Value::string("cover.png"),
                    )]))]),
                ),
                (
                    "chapterInfo".into(),
                    Value::array(vec![Value::object(HashMap::from([
                        ("startTime".into(), Value::Number(30.0)),
                        ("title".into(), Value::string("Chapter 2")),
                    ]))]),
                ),
            ]))],
        );
        let chapter = metadata.get_property("chapterInfo").get_property("0");
        assert!(w3cos_core::class::instance_of(
            &chapter,
            &chapter_information_class()
        ));
        assert_eq!(chapter.get_property("startTime").to_number(), 30.0);
        assert_eq!(chapter.get_property("title").to_js_string(), "Chapter 2");
        let session = media_session_value();
        assert!(w3cos_core::class::instance_of(
            &session,
            &media_session_class()
        ));
        session.set_property("metadata", metadata.clone());
        session.set_property("playbackState", Value::string("playing"));
        assert!(session.get_property("metadata").strict_eq(&metadata));
        assert_eq!(
            session.get_property("playbackState").to_js_string(),
            "playing"
        );

        let calls = Rc::new(Cell::new(0));
        let calls_for_handler = Rc::clone(&calls);
        session.call_method(
            "setActionHandler",
            vec![
                Value::string("play"),
                Value::function(move |_, args| {
                    assert_eq!(args[0].get_property("action").to_js_string(), "play");
                    calls_for_handler.set(calls_for_handler.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        assert!(dispatch_action("play", Value::object(HashMap::new())));
        assert_eq!(calls.get(), 1);

        session.call_method(
            "setPositionState",
            vec![Value::object(HashMap::from([
                ("duration".into(), Value::Number(120.0)),
                ("playbackRate".into(), Value::Number(1.5)),
                ("position".into(), Value::Number(30.0)),
            ]))],
        );
        assert_eq!(
            current_position_state(),
            Some(PositionState {
                duration: 120.0,
                playback_rate: 1.5,
                position: 30.0,
            })
        );
    }
}
