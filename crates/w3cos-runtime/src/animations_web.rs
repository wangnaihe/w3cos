//! Web Animations API state/lifecycle compatibility layer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static ANIMATIONS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static ANIMATION_INSTANCES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static EFFECTS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static TIMELINES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static TRIGGERS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static RANGE_LISTS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn realm_animation_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn warn_renderer() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: Web Animations preserves timing and lifecycle state; applying \
                 arbitrary keyframes to the native renderer requires compositor integration"
            );
        }
    });
}

fn illegal(name: &'static str) -> Value {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(&format!("Illegal constructor: {name}"))],
    ))
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(message)],
    ))
}

fn build_class(name: &'static str) -> Value {
    let class = match name {
        "Animation" => realm_animation_function(|_, args| {
            animation_value(
                args.first().cloned().unwrap_or(Value::Null),
                args.get(1).cloned().unwrap_or_else(document_timeline_value),
            )
        }),
        "KeyframeEffect" => realm_animation_function(|_, args| {
            keyframe_effect_value(
                args.first().cloned().unwrap_or(Value::Null),
                args.get(1).cloned().unwrap_or(Value::Undefined),
                args.get(2).cloned().unwrap_or(Value::Undefined),
            )
        }),
        "DocumentTimeline" => realm_animation_function(|_, args| {
            let origin_time = args
                .first()
                .map(|options| options.get_property("originTime").to_number())
                .unwrap_or_default();
            timeline_value("DocumentTimeline", Value::Null, "", origin_time)
        }),
        "ScrollTimeline" => realm_animation_function(|_, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            timeline_value(
                "ScrollTimeline",
                options.get_property("source"),
                &options.get_property("axis").to_js_string(),
                0.0,
            )
        }),
        "ViewTimeline" => realm_animation_function(|_, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            let value = timeline_value(
                "ViewTimeline",
                options.get_property("subject"),
                &options.get_property("axis").to_js_string(),
                0.0,
            );
            value.set_property("subject", options.get_property("subject"));
            value.set_property("startOffset", Value::Null);
            value.set_property("endOffset", Value::Null);
            value
        }),
        "TimelineTrigger" => realm_animation_function(|_, args| {
            timeline_trigger_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        _ => realm_animation_function(move |_, _| illegal(name)),
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    let members: &[&str] = match name {
        "Animation" => &[
            "cancel",
            "commitStyles",
            "currentTime",
            "effect",
            "finish",
            "finished",
            "id",
            "oncancel",
            "onfinish",
            "onremove",
            "overallProgress",
            "pause",
            "pending",
            "persist",
            "play",
            "playState",
            "playbackRate",
            "rangeEnd",
            "rangeStart",
            "ready",
            "replaceState",
            "reverse",
            "startTime",
            "timeline",
            "updatePlaybackRate",
        ],
        "AnimationEffect" => &["getComputedTiming", "getTiming", "updateTiming"],
        "AnimationTimeline" => &["currentTime", "duration"],
        "CSSAnimation" => &["animationName"],
        "CSSTransition" => &["transitionProperty"],
        "KeyframeEffect" => &[
            "composite",
            "getKeyframes",
            "pseudoElement",
            "setKeyframes",
            "target",
        ],
        "ScrollTimeline" => &["axis", "source"],
        "AnimationTrigger" => &["addAnimation", "getAnimations", "removeAnimation"],
        "TimelineTrigger" => &["ranges"],
        "TimelineTriggerRange" => &[
            "activationRangeEnd",
            "activationRangeStart",
            "activeRangeEnd",
            "activeRangeStart",
            "timeline",
        ],
        "TimelineTriggerRangeList" => &["entries", "forEach", "item", "keys", "length", "values"],
        "ViewTimeline" => &["endOffset", "startOffset", "subject"],
        _ => &[],
    };
    for member in members {
        prototype.set_property(member, Value::Undefined);
    }
    let parent = match name {
        "Animation" => Some(crate::web_events::event_target_class().get_property("prototype")),
        "AnimationEffect" => None,
        "AnimationTimeline" => None,
        "CSSAnimation" | "CSSTransition" => Some(class_for("Animation").get_property("prototype")),
        "KeyframeEffect" => Some(class_for("AnimationEffect").get_property("prototype")),
        "DocumentTimeline" | "ScrollTimeline" | "ViewTimeline" => {
            Some(class_for("AnimationTimeline").get_property("prototype"))
        }
        "TimelineTrigger" => Some(class_for("AnimationTrigger").get_property("prototype")),
        _ => None,
    };
    if let Some(parent) = parent {
        w3cos_core::class::set_prototype_of(&prototype, &parent);
    }
    class.set_property("prototype", prototype);
    class
}

fn range_property(init: &Value, name: &str, fallback: Value) -> Value {
    let value = init.get_property(name);
    if value.is_undefined() {
        fallback
    } else {
        value
    }
}

fn timeline_trigger_range_value(init: Value) -> Value {
    if !init.is_object() {
        type_error("TimelineTrigger range entries must be objects");
    }
    let timeline = init.get_property("timeline");
    if !timeline.is_null()
        && !w3cos_core::class::instance_of(&timeline, &class_for("AnimationTimeline"))
    {
        type_error("TimelineTrigger range timeline must be an AnimationTimeline or null");
    }
    let activation_start = range_property(&init, "activationRangeStart", Value::string("normal"));
    let activation_end = range_property(&init, "activationRangeEnd", Value::string("normal"));
    let active_start = range_property(&init, "activeRangeStart", activation_start.clone());
    let active_end = range_property(&init, "activeRangeEnd", activation_end.clone());
    let range = Value::object(HashMap::from([
        ("activationRangeEnd".into(), activation_end),
        ("activationRangeStart".into(), activation_start),
        ("activeRangeEnd".into(), active_end),
        ("activeRangeStart".into(), active_start),
        ("timeline".into(), timeline),
    ]));
    w3cos_core::class::set_prototype_of(
        &range,
        &class_for("TimelineTriggerRange").get_property("prototype"),
    );
    range
}

fn timeline_trigger_range_list_value(ranges: Rc<Vec<Value>>) -> Value {
    let list = Value::object(HashMap::new());
    list.set_property("length", Value::Number(ranges.len() as f64));
    for (index, range) in ranges.iter().enumerate() {
        list.set_property(&index.to_string(), range.clone());
    }
    let item_ranges = Rc::clone(&ranges);
    list.set_property(
        "item",
        realm_animation_function(move |_, args| {
            item_ranges
                .get(args.first().map(Value::to_u32).unwrap_or_default() as usize)
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    for method in ["keys", "values", "entries"] {
        let iterator_ranges = Rc::clone(&ranges);
        list.set_property(
            method,
            realm_animation_function(move |_, _| {
                let values = iterator_ranges
                    .iter()
                    .enumerate()
                    .map(|(index, range)| match method {
                        "keys" => Value::Number(index as f64),
                        "entries" => Value::array(vec![Value::Number(index as f64), range.clone()]),
                        _ => range.clone(),
                    })
                    .collect();
                Value::array(values).call_method("__w3cos_symbol_iterator", Vec::new())
            }),
        );
    }
    let each_ranges = Rc::clone(&ranges);
    let each_list = list.clone();
    list.set_property(
        "forEach",
        realm_animation_function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                type_error("TimelineTriggerRangeList.forEach requires a callback");
            }
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            for (index, range) in each_ranges.iter().enumerate() {
                callback.call(
                    this_arg.clone(),
                    vec![
                        range.clone(),
                        Value::Number(index as f64),
                        each_list.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &list,
        &class_for("TimelineTriggerRangeList").get_property("prototype"),
    );
    register_weak_realm_object(&RANGE_LISTS, &list);
    list
}

fn valid_animation_action(action: &str) -> bool {
    matches!(
        action,
        "none"
            | "pause"
            | "play"
            | "play-backwards"
            | "play-forwards"
            | "play-once"
            | "replay"
            | "reset"
    )
}

fn timeline_trigger_value(input: Value) -> Value {
    if !input.is_array() {
        type_error("TimelineTrigger requires an iterable of range options");
    }
    let ranges = Rc::new(
        input
            .iter()
            .map(timeline_trigger_range_value)
            .collect::<Vec<_>>(),
    );
    let associations = Rc::new(RefCell::new(Vec::<(Value, String, String)>::new()));
    let trigger = Value::object(HashMap::new());
    trigger.set_property(
        "ranges",
        timeline_trigger_range_list_value(Rc::clone(&ranges)),
    );

    let add_associations = Rc::clone(&associations);
    trigger.set_property(
        "addAnimation",
        realm_animation_function(move |_, args| {
            if args.len() < 2 {
                type_error("AnimationTrigger.addAnimation requires an animation and entry action");
            }
            let animation = args[0].clone();
            if !w3cos_core::class::instance_of(&animation, &class_for("Animation")) {
                type_error("AnimationTrigger.addAnimation requires an Animation");
            }
            let entry = args[1].to_js_string();
            let exit = args
                .get(2)
                .map(Value::to_js_string)
                .unwrap_or_else(|| "none".to_string());
            if !valid_animation_action(&entry) || !valid_animation_action(&exit) {
                type_error("AnimationTrigger action is not a supported animation action");
            }
            let mut associations = add_associations.borrow_mut();
            if let Some(existing) = associations
                .iter_mut()
                .find(|(candidate, _, _)| candidate.strict_eq(&animation))
            {
                existing.1 = entry;
                existing.2 = exit;
            } else {
                associations.push((animation, entry, exit));
            }
            warn_renderer();
            Value::Undefined
        }),
    );

    let get_associations = Rc::clone(&associations);
    trigger.set_property(
        "getAnimations",
        realm_animation_function(move |_, _| {
            Value::array(
                get_associations
                    .borrow()
                    .iter()
                    .map(|(animation, _, _)| animation.clone())
                    .collect(),
            )
        }),
    );

    trigger.set_property(
        "removeAnimation",
        realm_animation_function(move |_, args| {
            let animation = args.first().cloned().unwrap_or(Value::Undefined);
            associations
                .borrow_mut()
                .retain(|(candidate, _, _)| !candidate.strict_eq(&animation));
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &trigger,
        &class_for("TimelineTrigger").get_property("prototype"),
    );
    register_weak_realm_object(&TRIGGERS, &trigger);
    trigger
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

fn timeline_value(name: &'static str, source: Value, axis: &str, origin_time: f64) -> Value {
    let value = Value::object(HashMap::from([
        (
            "__w3cos_getter_currentTime".into(),
            realm_animation_function(move |_, _| {
                Value::Number(crate::jsdom::performance_now() - origin_time)
            }),
        ),
        ("duration".into(), Value::Null),
    ]));
    if name == "ScrollTimeline" {
        value.set_property(
            "axis",
            Value::string(if axis.is_empty() { "block" } else { axis }),
        );
        value.set_property("source", source);
    }
    w3cos_core::class::set_prototype_of(&value, &class_for(name).get_property("prototype"));
    register_weak_realm_object(&TIMELINES, &value);
    value
}

pub fn document_timeline_value() -> Value {
    timeline_value("DocumentTimeline", Value::Null, "", 0.0)
}

fn timing_from(options: Value) -> Value {
    let duration = if options.is_object() {
        let duration = options.get_property("duration");
        if duration.is_undefined() {
            Value::string("auto")
        } else {
            duration
        }
    } else if options.is_undefined() {
        Value::string("auto")
    } else {
        Value::Number(options.to_number().max(0.0))
    };
    let property = |name: &str, fallback: Value| {
        if options.is_object() {
            let value = options.get_property(name);
            if !value.is_undefined() {
                return value;
            }
        }
        fallback
    };
    Value::object(HashMap::from([
        ("delay".into(), property("delay", Value::Number(0.0))),
        (
            "direction".into(),
            property("direction", Value::string("normal")),
        ),
        ("duration".into(), duration),
        ("easing".into(), property("easing", Value::string("linear"))),
        ("endDelay".into(), property("endDelay", Value::Number(0.0))),
        ("fill".into(), property("fill", Value::string("auto"))),
        (
            "iterationStart".into(),
            property("iterationStart", Value::Number(0.0)),
        ),
        (
            "iterations".into(),
            property("iterations", Value::Number(1.0)),
        ),
    ]))
}

fn timing_copy(timing: &Value) -> Value {
    let mut properties = HashMap::new();
    for name in [
        "delay",
        "direction",
        "duration",
        "easing",
        "endDelay",
        "fill",
        "iterationStart",
        "iterations",
    ] {
        properties.insert(name.to_string(), timing.get_property(name));
    }
    Value::object(properties)
}

pub fn keyframe_effect_value(target: Value, keyframes: Value, options: Value) -> Value {
    let frames = Rc::new(RefCell::new(if keyframes.is_undefined() {
        Vec::new()
    } else {
        keyframes.iter().collect()
    }));
    let timing = Rc::new(RefCell::new(timing_from(options)));
    let effect = Value::object(HashMap::from([
        ("target".into(), target),
        ("pseudoElement".into(), Value::Null),
        ("composite".into(), Value::string("replace")),
    ]));
    let get_frames = Rc::clone(&frames);
    effect.set_property(
        "getKeyframes",
        realm_animation_function(move |_, _| Value::array(get_frames.borrow().clone())),
    );
    let set_frames = Rc::clone(&frames);
    effect.set_property(
        "setKeyframes",
        realm_animation_function(move |_, args| {
            *set_frames.borrow_mut() = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .iter()
                .collect();
            warn_renderer();
            Value::Undefined
        }),
    );
    let get_timing = Rc::clone(&timing);
    effect.set_property(
        "getTiming",
        realm_animation_function(move |_, _| timing_copy(&get_timing.borrow())),
    );
    let computed_timing = Rc::clone(&timing);
    effect.set_property(
        "getComputedTiming",
        realm_animation_function(move |_, _| {
            let value = timing_copy(&computed_timing.borrow());
            let duration = value.get_property("duration").to_number();
            let iterations = value.get_property("iterations").to_number();
            value.set_property("activeDuration", Value::Number(duration * iterations));
            value.set_property("endTime", Value::Number(duration * iterations));
            value.set_property("localTime", Value::Null);
            value.set_property("progress", Value::Null);
            value.set_property("currentIteration", Value::Null);
            value
        }),
    );
    let update_timing = timing;
    effect.set_property(
        "updateTiming",
        realm_animation_function(move |_, args| {
            let update = args.first().cloned().unwrap_or(Value::Undefined);
            let current = update_timing.borrow().clone();
            for name in [
                "delay",
                "direction",
                "duration",
                "easing",
                "endDelay",
                "fill",
                "iterationStart",
                "iterations",
            ] {
                let value = update.get_property(name);
                if !value.is_undefined() {
                    current.set_property(name, value);
                }
            }
            warn_renderer();
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &effect,
        &class_for("KeyframeEffect").get_property("prototype"),
    );
    register_weak_realm_object(&EFFECTS, &effect);
    effect
}

fn dispatch(animation: &Value, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("AnimationPlaybackEvent"),
        vec![Value::string(event_type)],
    );
    animation.call_method("dispatchEvent", vec![event]);
}

pub fn animation_value(effect: Value, timeline: Value) -> Value {
    let animation =
        w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    for (name, value) in [
        ("currentTime", Value::Number(0.0)),
        ("effect", effect),
        ("id", Value::string("")),
        ("oncancel", Value::Null),
        ("onfinish", Value::Null),
        ("onremove", Value::Null),
        ("overallProgress", Value::Number(0.0)),
        ("pending", Value::Bool(false)),
        ("playbackRate", Value::Number(1.0)),
        ("playState", Value::string("idle")),
        ("rangeEnd", Value::string("normal")),
        ("rangeStart", Value::string("normal")),
        ("replaceState", Value::string("active")),
        ("startTime", Value::Null),
        ("timeline", timeline),
    ] {
        animation.set_property(name, value);
    }
    animation.set_property(
        "ready",
        w3cos_core::promise::resolve(vec![animation.clone()]),
    );
    animation.set_property(
        "finished",
        w3cos_core::promise::resolve(vec![animation.clone()]),
    );
    for (method, state) in [
        ("play", "running"),
        ("pause", "paused"),
        ("reverse", "running"),
    ] {
        let value = animation.clone();
        animation.set_property(
            method,
            realm_animation_function(move |_, _| {
                value.set_property("playState", Value::string(state));
                if method == "reverse" {
                    value.set_property(
                        "playbackRate",
                        Value::Number(-value.get_property("playbackRate").to_number().abs()),
                    );
                }
                warn_renderer();
                Value::Undefined
            }),
        );
    }
    let cancel_value = animation.clone();
    animation.set_property(
        "cancel",
        realm_animation_function(move |_, _| {
            cancel_value.set_property("playState", Value::string("idle"));
            cancel_value.set_property("currentTime", Value::Null);
            dispatch(&cancel_value, "cancel");
            Value::Undefined
        }),
    );
    let finish_value = animation.clone();
    animation.set_property(
        "finish",
        realm_animation_function(move |_, _| {
            finish_value.set_property("playState", Value::string("finished"));
            finish_value.set_property("overallProgress", Value::Number(1.0));
            dispatch(&finish_value, "finish");
            Value::Undefined
        }),
    );
    let update_rate = animation.clone();
    animation.set_property(
        "updatePlaybackRate",
        realm_animation_function(move |_, args| {
            update_rate.set_property(
                "playbackRate",
                Value::Number(args.first().map(Value::to_number).unwrap_or(1.0)),
            );
            Value::Undefined
        }),
    );
    animation.set_property(
        "commitStyles",
        realm_animation_function(|_, _| {
            warn_renderer();
            Value::Undefined
        }),
    );
    animation.set_property("persist", realm_animation_function(|_, _| Value::Undefined));
    w3cos_core::class::set_prototype_of(
        &animation,
        &class_for("Animation").get_property("prototype"),
    );
    register_weak_realm_object(&ANIMATION_INSTANCES, &animation);
    animation
}

pub fn animate_element(target: Value, keyframes: Value, options: Value, node: u32) -> Value {
    let effect = keyframe_effect_value(target, keyframes, options);
    let animation = animation_value(effect, document_timeline_value());
    animation.set_property("__w3cos_animation_target", Value::Number(node as f64));
    animation.call_method("play", Vec::new());
    ANIMATIONS.with(|animations| animations.borrow_mut().push(animation.clone()));
    animation
}

pub(crate) fn discrete_property_sample(
    node: u32,
    property: &str,
) -> Option<(String, String, f64, String)> {
    let key = if property == "float" {
        "cssFloat".to_string()
    } else {
        let mut key = String::new();
        let mut uppercase_next = false;
        for character in property.chars() {
            if character == '-' {
                uppercase_next = true;
            } else if uppercase_next {
                key.extend(character.to_uppercase());
                uppercase_next = false;
            } else {
                key.push(character);
            }
        }
        key
    };
    ANIMATIONS.with(|animations| {
        animations.borrow().iter().rev().find_map(|animation| {
            if animation
                .get_property("__w3cos_animation_target")
                .to_u32()
                != node
            {
                return None;
            }
            let effect = animation.get_property("effect");
            let frames = effect.call_method("getKeyframes", Vec::new());
            let values = frames
                .iter()
                .filter_map(|frame| {
                    let value = frame.get_property(&key);
                    (!value.is_undefined()).then(|| value.to_js_string())
                })
                .collect::<Vec<_>>();
            let (Some(from), Some(to)) = (values.first(), values.last()) else {
                return None;
            };
            let timing = effect.call_method("getTiming", Vec::new());
            let duration = timing.get_property("duration").to_number();
            if !duration.is_finite() || duration <= 0.0 {
                return None;
            }
            let delay = timing.get_property("delay").to_number();
            let current_time = animation.get_property("currentTime").to_number();
            let progress = ((current_time - delay) / duration).clamp(0.0, 1.0);
            Some((
                from.clone(),
                to.clone(),
                progress,
                timing.get_property("easing").to_js_string(),
            ))
        })
    })
}

/// Create (or return) the Web Animations facade corresponding to one CSS
/// animation/transition. CSS-driven effects share `getAnimations()` with
/// script-created effects, as required by the Web Animations integration.
pub(crate) fn css_motion_animation(
    node: u32,
    kind: &str,
    label: &str,
    pseudo: Option<&str>,
    property: &str,
) -> Value {
    let pseudo = pseudo.unwrap_or_default();
    if let Some(existing) = ANIMATIONS.with(|animations| {
        animations
            .borrow()
            .iter()
            .find(|animation| {
                animation.get_property("__w3cos_animation_target").to_u32() == node
                    && animation
                        .get_property("__w3cos_css_motion_kind")
                        .to_js_string()
                        == kind
                    && animation
                        .get_property("__w3cos_css_motion_pseudo")
                        .to_js_string()
                        == pseudo
                    && animation
                        .get_property("__w3cos_css_motion_property")
                        .to_js_string()
                        == property
            })
            .cloned()
    }) {
        return existing;
    }

    let effect = keyframe_effect_value(
        crate::jsdom::element_value(node),
        Value::array(Vec::new()),
        Value::Undefined,
    );
    if !pseudo.is_empty() {
        effect.set_property("pseudoElement", Value::string(pseudo));
    }
    let animation = animation_value(effect, document_timeline_value());
    animation.set_property("__w3cos_animation_target", Value::Number(node as f64));
    animation.set_property("__w3cos_css_motion_kind", Value::string(kind));
    animation.set_property("__w3cos_css_motion_pseudo", Value::string(pseudo));
    animation.set_property("__w3cos_css_motion_property", Value::string(property));
    if kind == "animation" {
        animation.set_property("animationName", Value::string(label));
        w3cos_core::class::set_prototype_of(
            &animation,
            &class_for("CSSAnimation").get_property("prototype"),
        );
    } else {
        animation.set_property("transitionProperty", Value::string(label));
        w3cos_core::class::set_prototype_of(
            &animation,
            &class_for("CSSTransition").get_property("prototype"),
        );
    }
    let commit_pseudo = pseudo.to_string();
    let commit_property = property.to_string();
    animation.set_property(
        "commitStyles",
        realm_animation_function(move |_, _| {
            crate::jsdom::commit_css_motion_style(
                node,
                (!commit_pseudo.is_empty()).then_some(commit_pseudo.as_str()),
                &commit_property,
            );
            Value::Undefined
        }),
    );
    animation.call_method("play", Vec::new());
    ANIMATIONS.with(|animations| animations.borrow_mut().push(animation.clone()));
    animation
}

pub fn animations_for(node: Option<u32>, subtree: bool) -> Value {
    Value::array(ANIMATIONS.with(|animations| {
        animations
            .borrow()
            .iter()
            .filter(|animation| {
                let target = animation.get_property("__w3cos_animation_target").to_u32();
                node.is_none_or(|node| {
                    target == node || (subtree && crate::jsdom::is_ancestor_node(node, target))
                })
            })
            .cloned()
            .collect()
    }))
}

pub fn reset() {
    ANIMATIONS.with(|animations| animations.borrow_mut().clear());
    ANIMATION_INSTANCES.with(|animations| {
        for animation in animations
            .borrow_mut()
            .drain(..)
            .filter_map(|animation| upgrade_realm_object(&animation))
        {
            animation.set_property("playState", Value::string("idle"));
            animation.set_property("pending", Value::Bool(false));
            for callback in ["oncancel", "onfinish", "onremove"] {
                animation.set_property(callback, Value::Null);
            }
            for reference in ["effect", "finished", "ready", "timeline"] {
                animation.set_property(reference, Value::Undefined);
            }
            for method in [
                "cancel",
                "commitStyles",
                "finish",
                "pause",
                "persist",
                "play",
                "reverse",
                "updatePlaybackRate",
            ] {
                animation.set_property(method, Value::Undefined);
            }
        }
    });
    EFFECTS.with(|effects| {
        for effect in effects
            .borrow_mut()
            .drain(..)
            .filter_map(|effect| upgrade_realm_object(&effect))
        {
            effect.set_property("target", Value::Undefined);
            for method in [
                "getComputedTiming",
                "getKeyframes",
                "getTiming",
                "setKeyframes",
                "updateTiming",
            ] {
                effect.set_property(method, Value::Undefined);
            }
        }
    });
    TIMELINES.with(|timelines| {
        for timeline in timelines
            .borrow_mut()
            .drain(..)
            .filter_map(|timeline| upgrade_realm_object(&timeline))
        {
            timeline.set_property("source", Value::Undefined);
            timeline.set_property("subject", Value::Undefined);
            timeline.set_property("__w3cos_getter_currentTime", Value::Undefined);
        }
    });
    TRIGGERS.with(|triggers| {
        for trigger in triggers
            .borrow_mut()
            .drain(..)
            .filter_map(|trigger| upgrade_realm_object(&trigger))
        {
            trigger.set_property("ranges", Value::Undefined);
            for method in ["addAnimation", "getAnimations", "removeAnimation"] {
                trigger.set_property(method, Value::Undefined);
            }
        }
    });
    RANGE_LISTS.with(|lists| {
        for list in lists
            .borrow_mut()
            .drain(..)
            .filter_map(|list| upgrade_realm_object(&list))
        {
            let length = list.get_property("length").to_u32();
            for index in 0..length {
                list.set_property(&index.to_string(), Value::Undefined);
            }
            list.set_property("length", Value::Number(0.0));
            for method in ["entries", "forEach", "item", "keys", "values"] {
                list.set_property(method, Value::Undefined);
            }
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        disconnect_realm_class(class);
    }
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_lifecycle_and_effect_timing_are_stateful() {
        reset();
        let effect = keyframe_effect_value(
            Value::Null,
            Value::array(vec![Value::object(HashMap::from([(
                "opacity".into(),
                Value::Number(0.0),
            )]))]),
            Value::Number(250.0),
        );
        assert_eq!(
            effect
                .call_method("getTiming", Vec::new())
                .get_property("duration")
                .to_number(),
            250.0
        );
        let animation = animation_value(effect, document_timeline_value());
        animation.call_method("play", Vec::new());
        assert_eq!(
            animation.get_property("playState").to_js_string(),
            "running"
        );
        animation.call_method("finish", Vec::new());
        assert_eq!(
            animation.get_property("playState").to_js_string(),
            "finished"
        );
    }

    #[test]
    fn animations_effects_timelines_triggers_and_callbacks_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_animation_class = class_for("Animation");
        let old_effect_class = class_for("KeyframeEffect");
        let old_trigger_class = class_for("TimelineTrigger");
        let target = Value::object(HashMap::new());
        let target_weak = crate::jsdom::weak_realm_object(&target);
        let effect = keyframe_effect_value(
            target.clone(),
            Value::array(vec![Value::object(HashMap::from([(
                "opacity".into(),
                Value::Number(0.0),
            )]))]),
            Value::Number(100.0),
        );
        drop(target);
        let timeline = document_timeline_value();
        let animation = animation_value(effect.clone(), timeline.clone());
        let trigger = w3cos_core::class::construct(
            &old_trigger_class,
            vec![Value::array(vec![Value::object(HashMap::from([(
                "timeline".into(),
                timeline,
            )]))])],
        );
        trigger.call_method(
            "addAnimation",
            vec![animation.clone(), Value::string("play")],
        );
        let ranges = trigger.get_property("ranges");
        let animation_weak = crate::jsdom::weak_realm_object(&animation);
        let effect_weak = crate::jsdom::weak_realm_object(&effect);
        let trigger_weak = crate::jsdom::weak_realm_object(&trigger);
        let ranges_weak = crate::jsdom::weak_realm_object(&ranges);

        let finish_marker = Rc::new(());
        let finish_marker_weak = Rc::downgrade(&finish_marker);
        animation.set_property(
            "onfinish",
            Value::function(move |_, _| {
                let _ = &finish_marker;
                Value::Undefined
            }),
        );
        animation.call_method("play", Vec::new());

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_animation_class.strict_eq(&class_for("Animation")));
        assert!(!old_effect_class.strict_eq(&class_for("KeyframeEffect")));
        assert!(!old_trigger_class.strict_eq(&class_for("TimelineTrigger")));
        for class in [old_animation_class, old_effect_class, old_trigger_class] {
            assert!(class.call(Value::Undefined, Vec::new()).is_undefined());
        }
        assert_eq!(animation.get_property("playState").to_js_string(), "idle");
        assert!(animation.get_property("effect").is_undefined());
        assert!(animation.get_property("timeline").is_undefined());
        assert!(animation.get_property("ready").is_undefined());
        assert!(animation.get_property("finished").is_undefined());
        assert!(animation.get_property("onfinish").is_null());
        assert!(animation.call_method("play", Vec::new()).is_undefined());
        assert!(effect.get_property("target").is_undefined());
        assert!(effect.call_method("getTiming", Vec::new()).is_undefined());
        assert!(trigger.get_property("ranges").is_undefined());
        assert!(
            trigger
                .call_method("getAnimations", Vec::new())
                .is_undefined()
        );
        assert_eq!(ranges.get_property("length").to_number(), 0.0);
        assert!(ranges.call_method("item", Vec::new()).is_undefined());
        assert!(target_weak.upgrade().is_none());
        assert!(finish_marker_weak.upgrade().is_none());

        drop(animation);
        drop(effect);
        drop(trigger);
        drop(ranges);
        assert!(animation_weak.upgrade().is_none());
        assert!(effect_weak.upgrade().is_none());
        assert!(trigger_weak.upgrade().is_none());
        assert!(ranges_weak.upgrade().is_none());
    }

    #[test]
    fn timeline_trigger_retains_ranges_and_unique_animation_associations() {
        reset();
        let timeline = w3cos_core::class::construct(&class_for("ScrollTimeline"), Vec::new());
        let trigger = w3cos_core::class::construct(
            &class_for("TimelineTrigger"),
            vec![Value::array(vec![Value::object(HashMap::from([
                ("timeline".into(), timeline.clone()),
                ("activationRangeStart".into(), Value::string("10%")),
                ("activationRangeEnd".into(), Value::string("20%")),
                ("activeRangeStart".into(), Value::string("5%")),
                ("activeRangeEnd".into(), Value::string("25%")),
            ]))])],
        );
        assert!(w3cos_core::class::instance_of(
            &trigger,
            &class_for("AnimationTrigger")
        ));
        let ranges = trigger.get_property("ranges");
        assert_eq!(ranges.get_property("length").to_number(), 1.0);
        assert_eq!(
            ranges
                .call_method("item", vec![Value::Number(0.0)])
                .get_property("activationRangeStart")
                .to_js_string(),
            "10%"
        );
        assert!(
            ranges
                .get_property("0")
                .get_property("timeline")
                .strict_eq(&timeline)
        );

        let animation = animation_value(Value::Null, document_timeline_value());
        trigger.call_method(
            "addAnimation",
            vec![animation.clone(), Value::string("play-forwards")],
        );
        trigger.call_method(
            "addAnimation",
            vec![
                animation.clone(),
                Value::string("play"),
                Value::string("pause"),
            ],
        );
        assert_eq!(
            trigger
                .call_method("getAnimations", Vec::new())
                .get_property("length")
                .to_number(),
            1.0
        );
        trigger.call_method("removeAnimation", vec![animation]);
        assert_eq!(
            trigger
                .call_method("getAnimations", Vec::new())
                .get_property("length")
                .to_number(),
            0.0
        );
    }
}
