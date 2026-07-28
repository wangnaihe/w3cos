//! HTML media text-track compatibility objects.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
}

const CLASS_NAMES: &[&str] = &[
    "TextTrack",
    "TextTrackCue",
    "TextTrackCueList",
    "TextTrackList",
    "VTTCue",
    "VideoPlaybackQuality",
];

fn realm_text_track_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn register_text_track_value(value: &Value) {
    register_weak_realm_object(&VALUES, value);
}

fn illegal(name: &'static str) -> Value {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(&format!("Illegal constructor: {name}"))],
    ))
}

fn build_class(name: &'static str) -> Value {
    let class = if name == "VTTCue" {
        realm_text_track_function(|_, args| {
            cue_value(
                args.first().map(Value::to_number).unwrap_or_default(),
                args.get(1).map(Value::to_number).unwrap_or_default(),
                &args.get(2).map(Value::to_js_string).unwrap_or_default(),
            )
        })
    } else {
        realm_text_track_function(move |_, _| illegal(name))
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in class_members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    let parent = match name {
        "TextTrack" | "TextTrackCue" | "TextTrackList" => {
            Some(crate::web_events::event_target_class().get_property("prototype"))
        }
        "VTTCue" => Some(class_for("TextTrackCue").get_property("prototype")),
        _ => None,
    };
    if let Some(parent) = parent {
        w3cos_core::class::set_prototype_of(&prototype, &parent);
    }
    class.set_property("prototype", prototype);
    class
}

fn class_members(name: &str) -> &'static [&'static str] {
    match name {
        "TextTrack" => &[
            "activeCues",
            "addCue",
            "cues",
            "id",
            "kind",
            "label",
            "language",
            "mode",
            "oncuechange",
            "removeCue",
        ],
        "TextTrackCue" => &[
            "endTime",
            "id",
            "onenter",
            "onexit",
            "pauseOnExit",
            "startTime",
            "track",
        ],
        "TextTrackCueList" => &["getCueById", "length"],
        "TextTrackList" => &[
            "getTrackById",
            "length",
            "onaddtrack",
            "onchange",
            "onremovetrack",
        ],
        "VTTCue" => &[
            "align",
            "getCueAsHTML",
            "line",
            "position",
            "size",
            "snapToLines",
            "text",
            "vertical",
        ],
        "VideoPlaybackQuality" => &[
            "corruptedVideoFrames",
            "creationTime",
            "droppedVideoFrames",
            "totalVideoFrames",
        ],
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

fn refresh_indexed(list: &Value, values: &[Value], old_length: usize) {
    for index in 0..old_length.max(values.len()) {
        list.set_property(
            &index.to_string(),
            values.get(index).cloned().unwrap_or(Value::Undefined),
        );
    }
    list.set_property("length", Value::Number(values.len() as f64));
}

fn cue_list_value(state: Rc<RefCell<Vec<Value>>>) -> Value {
    let list = Value::object(HashMap::new());
    refresh_indexed(&list, &state.borrow(), 0);
    list.set_property(
        "getCueById",
        realm_text_track_function(move |_, args| {
            let id = args.first().map(Value::to_js_string).unwrap_or_default();
            state
                .borrow()
                .iter()
                .find(|cue| cue.get_property("id").to_js_string() == id)
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    w3cos_core::class::set_prototype_of(
        &list,
        &class_for("TextTrackCueList").get_property("prototype"),
    );
    register_text_track_value(&list);
    list
}

pub fn text_track_value(kind: &str, label: &str, language: &str) -> Value {
    let cues = Rc::new(RefCell::new(Vec::<Value>::new()));
    let cue_list = cue_list_value(Rc::clone(&cues));
    let active_cues = cue_list_value(Rc::clone(&cues));
    let track = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    for (name, value) in [
        ("activeCues", active_cues.clone()),
        ("cues", cue_list.clone()),
        ("id", Value::string("")),
        ("kind", Value::string(kind)),
        ("label", Value::string(label)),
        ("language", Value::string(language)),
        ("mode", Value::string("disabled")),
        ("oncuechange", Value::Null),
    ] {
        track.set_property(name, value);
    }
    let add_state = Rc::clone(&cues);
    let add_list = cue_list.clone();
    let add_active = active_cues.clone();
    let track_for_add = track.clone();
    track.set_property(
        "addCue",
        realm_text_track_function(move |_, args| {
            let cue = args.first().cloned().unwrap_or(Value::Undefined);
            let old_length = add_state.borrow().len();
            cue.set_property("track", track_for_add.clone());
            add_state.borrow_mut().push(cue);
            let values = add_state.borrow();
            refresh_indexed(&add_list, &values, old_length);
            refresh_indexed(&add_active, &values, old_length);
            Value::Undefined
        }),
    );
    let remove_state = Rc::clone(&cues);
    let remove_list = cue_list;
    let remove_active = active_cues;
    track.set_property(
        "removeCue",
        realm_text_track_function(move |_, args| {
            let cue = args.first().cloned().unwrap_or(Value::Undefined);
            let old_length = remove_state.borrow().len();
            remove_state
                .borrow_mut()
                .retain(|candidate| !candidate.strict_eq(&cue));
            let values = remove_state.borrow();
            refresh_indexed(&remove_list, &values, old_length);
            refresh_indexed(&remove_active, &values, old_length);
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&track, &class_for("TextTrack").get_property("prototype"));
    register_text_track_value(&track);
    track
}

pub fn text_track_list_value() -> Value {
    let tracks = Rc::new(RefCell::new(Vec::<Value>::new()));
    let list = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    for (name, value) in [
        ("length", Value::Number(0.0)),
        ("onaddtrack", Value::Null),
        ("onchange", Value::Null),
        ("onremovetrack", Value::Null),
    ] {
        list.set_property(name, value);
    }
    let lookup = Rc::clone(&tracks);
    list.set_property(
        "getTrackById",
        realm_text_track_function(move |_, args| {
            let id = args.first().map(Value::to_js_string).unwrap_or_default();
            lookup
                .borrow()
                .iter()
                .find(|track| track.get_property("id").to_js_string() == id)
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    let append_state = tracks;
    let list_for_append = list.clone();
    list.set_property(
        "__w3cos_append",
        realm_text_track_function(move |_, args| {
            let track = args.first().cloned().unwrap_or(Value::Undefined);
            let index = append_state.borrow().len();
            append_state.borrow_mut().push(track.clone());
            list_for_append.set_property(&index.to_string(), track.clone());
            list_for_append.set_property("length", Value::Number((index + 1) as f64));
            let event = w3cos_core::class::construct(
                &crate::web_events::event_class(),
                vec![Value::string("addtrack")],
            );
            event.set_property("track", track);
            list_for_append.call_method("dispatchEvent", vec![event]);
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &list,
        &class_for("TextTrackList").get_property("prototype"),
    );
    register_text_track_value(&list);
    list
}

pub fn append_track(list: &Value, track: Value) {
    list.call_method("__w3cos_append", vec![track]);
}

pub fn cue_value(start: f64, end: f64, text: &str) -> Value {
    if !start.is_finite() || !end.is_finite() || end < start {
        w3cos_core::throw_value(w3cos_core::error_instance(
            "TypeError",
            vec![Value::string("VTTCue requires a valid time range")],
        ));
    }
    let cue = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    for (name, value) in [
        ("align", Value::string("center")),
        ("endTime", Value::Number(end)),
        ("id", Value::string("")),
        ("line", Value::string("auto")),
        ("onenter", Value::Null),
        ("onexit", Value::Null),
        ("pauseOnExit", Value::Bool(false)),
        ("position", Value::string("auto")),
        ("size", Value::Number(100.0)),
        ("snapToLines", Value::Bool(true)),
        ("startTime", Value::Number(start)),
        ("text", Value::string(text)),
        ("track", Value::Null),
        ("vertical", Value::string("")),
    ] {
        cue.set_property(name, value);
    }
    cue.set_property(
        "getCueAsHTML",
        realm_text_track_function({
            let text = text.to_string();
            move |_, _| {
                let fragment = crate::jsdom::document_value()
                    .call_method("createDocumentFragment", Vec::new());
                let text_node = crate::jsdom::document_value()
                    .call_method("createTextNode", vec![Value::string(&text)]);
                fragment.call_method("appendChild", vec![text_node]);
                fragment
            }
        }),
    );
    w3cos_core::class::set_prototype_of(&cue, &class_for("VTTCue").get_property("prototype"));
    register_text_track_value(&cue);
    cue
}

pub fn playback_quality_value() -> Value {
    let value = Value::object(HashMap::from([
        ("corruptedVideoFrames".into(), Value::Number(0.0)),
        (
            "creationTime".into(),
            Value::Number(crate::jsdom::performance_now()),
        ),
        ("droppedVideoFrames".into(), Value::Number(0.0)),
        ("totalVideoFrames".into(), Value::Number(0.0)),
    ]));
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("VideoPlaybackQuality").get_property("prototype"),
    );
    register_text_track_value(&value);
    value
}

pub fn reset() {
    VALUES.with(|values| {
        for value in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            let length = value.get_property("length").to_u32() as usize;
            for index in 0..length {
                value.set_property(&index.to_string(), Value::Undefined);
            }
            for name in CLASS_NAMES {
                for member in class_members(name) {
                    value.set_property(member, Value::Undefined);
                }
            }
            value.set_property("__w3cos_append", Value::Undefined);
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        disconnect_realm_class(class);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_retain_cues_and_list_identity() {
        reset();
        let list = text_track_list_value();
        let track = text_track_value("subtitles", "English", "en");
        append_track(&list, track.clone());
        assert_eq!(list.get_property("length").to_number(), 1.0);
        let cue = cue_value(1.0, 2.0, "hello");
        track.call_method("addCue", vec![cue.clone()]);
        assert_eq!(
            track
                .get_property("cues")
                .get_property("length")
                .to_number(),
            1.0
        );
        assert!(cue.get_property("track").strict_eq(&track));
    }

    #[test]
    fn tracks_cues_lists_cycles_and_classes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_track_class = class_for("TextTrack");
        let old_cue_class = class_for("VTTCue");
        let list = text_track_list_value();
        let track = text_track_value("subtitles", "English", "en");
        let cue = cue_value(1.0, 2.0, "old realm");
        append_track(&list, track.clone());
        track.call_method("addCue", vec![cue.clone()]);
        let cue_list = track.get_property("cues");
        let cue_list_weak = crate::jsdom::weak_realm_object(&cue_list);
        drop(cue_list);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_track_class.get_property("prototype").is_undefined());
        assert!(old_cue_class.get_property("prototype").is_undefined());
        assert!(!old_track_class.strict_eq(&class_for("TextTrack")));
        assert!(list.get_property("getTrackById").is_undefined());
        assert!(list.get_property("0").is_undefined());
        assert!(track.get_property("addCue").is_undefined());
        assert!(track.get_property("cues").is_undefined());
        assert!(cue.get_property("track").is_undefined());
        assert!(cue.get_property("getCueAsHTML").is_undefined());
        assert!(cue_list_weak.upgrade().is_none());
    }
}
