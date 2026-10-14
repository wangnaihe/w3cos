//! Screen Orientation and Window Management screen-detail facades.

use std::cell::RefCell;
use std::collections::HashMap;
use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static ORIENTATION: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SCREEN: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DETAILS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ORIENTATION_STATE: RefCell<(String, f64)> =
        RefCell::new(("landscape-primary".to_string(), 0.0));
}

fn type_error(message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ]))
}

fn illegal_class(name: &'static str, parent: Option<Value>) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(type_error(&format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        if let Some(parent) = parent {
            w3cos_core::class::set_prototype_of(&prototype, &parent.get_property("prototype"));
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn screen_class() -> Value {
    illegal_class("Screen", Some(crate::web_events::event_target_class()))
}

pub fn screen_orientation_class() -> Value {
    illegal_class(
        "ScreenOrientation",
        Some(crate::web_events::event_target_class()),
    )
}

pub fn screen_detailed_class() -> Value {
    let class = illegal_class("ScreenDetailed", Some(screen_class()));
    let prototype = class.get_property("prototype");
    for name in [
        "devicePixelRatio",
        "isInternal",
        "isPrimary",
        "label",
        "left",
        "top",
    ] {
        prototype.set_property(name, Value::Undefined);
    }
    class
}

pub fn screen_details_class() -> Value {
    let class = illegal_class(
        "ScreenDetails",
        Some(crate::web_events::event_target_class()),
    );
    let prototype = class.get_property("prototype");
    for name in [
        "currentScreen",
        "oncurrentscreenchange",
        "onscreenschange",
        "screens",
    ] {
        prototype.set_property(name, Value::Undefined);
    }
    class
}

fn orientation_value() -> Value {
    ORIENTATION.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::new());
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        value.set_property(
            "__w3cos_getter_type",
            Value::function(|_, _| {
                ORIENTATION_STATE.with(|state| Value::string(&state.borrow().0))
            }),
        );
        value.set_property(
            "__w3cos_getter_angle",
            Value::function(|_, _| ORIENTATION_STATE.with(|state| Value::Number(state.borrow().1))),
        );
        let value_for_lock = value.clone();
        value.set_property(
            "lock",
            Value::function(move |_, args| {
                let requested = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let valid = matches!(
                    requested.as_str(),
                    "any"
                        | "natural"
                        | "landscape"
                        | "portrait"
                        | "portrait-primary"
                        | "portrait-secondary"
                        | "landscape-primary"
                        | "landscape-secondary"
                );
                if !valid {
                    return w3cos_core::promise::reject(vec![type_error(
                        "Invalid screen orientation lock type",
                    )]);
                }
                let normalized = match requested.as_str() {
                    "portrait" => "portrait-primary",
                    "landscape" | "any" | "natural" => "landscape-primary",
                    value => value,
                };
                let angle = if normalized.starts_with("portrait") {
                    90.0
                } else if normalized.ends_with("secondary") {
                    180.0
                } else {
                    0.0
                };
                ORIENTATION_STATE
                    .with(|state| *state.borrow_mut() = (normalized.to_string(), angle));
                let event = w3cos_core::class::construct(
                    &crate::web_events::event_class(),
                    vec![Value::string("change")],
                );
                value_for_lock.call_method("dispatchEvent", vec![event]);
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        let value_for_unlock = value.clone();
        value.set_property(
            "unlock",
            Value::function(move |_, _| {
                ORIENTATION_STATE
                    .with(|state| *state.borrow_mut() = ("landscape-primary".to_string(), 0.0));
                let event = w3cos_core::class::construct(
                    &crate::web_events::event_class(),
                    vec![Value::string("change")],
                );
                value_for_unlock.call_method("dispatchEvent", vec![event]);
                Value::Undefined
            }),
        );
        value.set_property("onchange", Value::Null);
        let prototype = screen_orientation_class().get_property("prototype");
        for name in ["angle", "type", "lock", "unlock", "onchange"] {
            let property = if matches!(name, "angle" | "type") {
                Value::Undefined
            } else {
                value.get_property(name)
            };
            prototype.set_property(name, property);
        }
        w3cos_core::class::set_prototype_of(&value, &prototype);
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn screen_value() -> Value {
    SCREEN.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::new());
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        for name in ["width", "height", "availWidth", "availHeight"] {
            let is_height = name.contains("Height") || name == "height";
            value.set_property(
                &format!("__w3cos_getter_{name}"),
                Value::function(move |_, _| {
                    let (width, height, _) = crate::jsdom::viewport();
                    Value::Number(if is_height { height } else { width })
                }),
            );
        }
        for name in ["availLeft", "availTop"] {
            value.set_property(&format!("__w3cos_getter_{name}"), Value::Number(0.0));
        }
        value.set_property("colorDepth", Value::Number(24.0));
        value.set_property("pixelDepth", Value::Number(24.0));
        value.set_property("isExtended", Value::Bool(false));
        value.set_property("orientation", orientation_value());
        value.set_property("onchange", Value::Null);
        let prototype = screen_class().get_property("prototype");
        for name in [
            "width",
            "height",
            "availWidth",
            "availHeight",
            "availLeft",
            "availTop",
            "colorDepth",
            "pixelDepth",
            "isExtended",
            "orientation",
            "onchange",
        ] {
            prototype.set_property(
                name,
                if matches!(
                    name,
                    "width" | "height" | "availWidth" | "availHeight" | "availLeft" | "availTop"
                ) {
                    Value::Undefined
                } else {
                    value.get_property(name)
                },
            );
        }
        w3cos_core::class::set_prototype_of(&value, &prototype);
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn detailed_screen_value() -> Value {
    let value = Value::object(HashMap::new());
    let screen = screen_value();
    for name in [
        "width",
        "height",
        "availWidth",
        "availHeight",
        "availLeft",
        "availTop",
    ] {
        value.set_property(
            &format!("__w3cos_getter_{name}"),
            screen.get_property(&format!("__w3cos_getter_{name}")),
        );
    }
    for name in [
        "colorDepth",
        "pixelDepth",
        "isExtended",
        "orientation",
        "onchange",
    ] {
        value.set_property(name, screen.get_property(name));
    }
    value.set_property("left", Value::Number(0.0));
    value.set_property("top", Value::Number(0.0));
    value.set_property("isPrimary", Value::Bool(true));
    value.set_property("isInternal", Value::Bool(true));
    value.set_property("label", Value::string("Primary display"));
    value.set_property(
        "__w3cos_getter_devicePixelRatio",
        Value::function(|_, _| Value::Number(crate::jsdom::viewport().2)),
    );
    let prototype = screen_detailed_class().get_property("prototype");
    for name in [
        "left",
        "top",
        "isPrimary",
        "isInternal",
        "label",
        "devicePixelRatio",
    ] {
        prototype.set_property(
            name,
            if name == "devicePixelRatio" {
                Value::Undefined
            } else {
                value.get_property(name)
            },
        );
    }
    w3cos_core::class::set_prototype_of(&value, &prototype);
    value
}

pub fn screen_details_value() -> Value {
    DETAILS.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let current = detailed_screen_value();
        let value = Value::object(HashMap::from([
            ("currentScreen".into(), current.clone()),
            ("screens".into(), Value::array(vec![current])),
            ("oncurrentscreenchange".into(), Value::Null),
            ("onscreenschange".into(), Value::Null),
        ]));
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        let prototype = screen_details_class().get_property("prototype");
        for name in [
            "currentScreen",
            "screens",
            "oncurrentscreenchange",
            "onscreenschange",
        ] {
            prototype.set_property(name, value.get_property(name));
        }
        w3cos_core::class::set_prototype_of(&value, &prototype);
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn detailed_from(display: &crate::platform_host::DisplayInfo, primary_fallback: bool) -> Value {
    let width = f64::from(display.width);
    let height = f64::from(display.height);
    let left = f64::from(display.left);
    let top = f64::from(display.top);
    let value = Value::object(HashMap::from([
        ("colorDepth".into(), Value::Number(24.0)),
        ("pixelDepth".into(), Value::Number(24.0)),
        (
            "isPrimary".into(),
            Value::Bool(display.primary || primary_fallback),
        ),
        ("isInternal".into(), Value::Bool(display.internal)),
        ("label".into(), Value::string(&display.label)),
        ("left".into(), Value::Number(left)),
        ("top".into(), Value::Number(top)),
        ("availLeft".into(), Value::Number(left)),
        ("availTop".into(), Value::Number(top)),
    ]));
    value.set_property(
        "__w3cos_getter_width",
        Value::function(move |_, _| Value::Number(width)),
    );
    value.set_property(
        "__w3cos_getter_height",
        Value::function(move |_, _| Value::Number(height)),
    );
    value.set_property(
        "__w3cos_getter_availWidth",
        Value::function(move |_, _| Value::Number(width)),
    );
    value.set_property(
        "__w3cos_getter_availHeight",
        Value::function(move |_, _| Value::Number(height)),
    );
    value.set_property(
        "__w3cos_getter_devicePixelRatio",
        Value::function(|_, _| Value::Number(crate::jsdom::viewport().2)),
    );
    w3cos_core::class::set_prototype_of(&value, &screen_detailed_class().get_property("prototype"));
    value
}

pub fn get_screen_details() -> Value {
    if let Some(rejected) = crate::capability_gate::reject_unless_granted("window-management") {
        return rejected;
    }
    match crate::platform_host::list_displays() {
        Ok(displays) => {
            let screens = displays
                .iter()
                .enumerate()
                .map(|(index, display)| detailed_from(display, index == 0))
                .collect::<Vec<_>>();
            let current = screens
                .first()
                .cloned()
                .unwrap_or_else(detailed_screen_value);
            let extended = screens.len() > 1;
            let value = Value::object(HashMap::from([
                ("currentScreen".into(), current),
                ("screens".into(), Value::array(screens)),
                ("oncurrentscreenchange".into(), Value::Null),
                ("onscreenschange".into(), Value::Null),
                ("isExtended".into(), Value::Bool(extended)),
            ]));
            crate::web_events::event_target_class().call(value.clone(), vec![]);
            w3cos_core::class::set_prototype_of(
                &value,
                &screen_details_class().get_property("prototype"),
            );
            w3cos_core::promise::resolve(vec![value])
        }
        Err(message) => {
            w3cos_core::promise::reject(vec![type_error_named("NotSupportedError", &message)])
        }
    }
}

fn type_error_named(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

/// Release Realm-owned screen wrappers and listeners while retaining the
/// platform orientation snapshot used to initialize the next Realm.
pub fn reset_realm() {
    DETAILS.with(|slot| {
        slot.borrow_mut().take();
    });
    SCREEN.with(|slot| {
        slot.borrow_mut().take();
    });
    ORIENTATION.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_live_single_screen_snapshot_and_orientation() {
        crate::jsdom::set_viewport(1280.0, 720.0);
        crate::jsdom::set_device_pixel_ratio(2.0);
        let screen = screen_value();
        assert!(w3cos_core::class::instance_of(&screen, &screen_class()));
        assert_eq!(screen.get_property("width").to_number(), 1280.0);
        let details = screen_details_value();
        let current = details.get_property("currentScreen");
        assert!(w3cos_core::class::instance_of(
            &current,
            &screen_detailed_class()
        ));
        assert_eq!(current.get_property("devicePixelRatio").to_number(), 2.0);
        assert_eq!(details.get_property("screens").iter().len(), 1);
        screen
            .get_property("orientation")
            .call_method("lock", vec![Value::string("portrait")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            screen
                .get_property("orientation")
                .get_property("type")
                .to_js_string(),
            "portrait-primary"
        );
    }

    #[test]
    fn window_management_lists_granted_displays_and_refuses_without_permission() {
        crate::capability_gate::reset();
        crate::platform_host::reset_overrides();
        let denied = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let denied_for_catch = std::rc::Rc::clone(&denied);
        get_screen_details().call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *denied_for_catch.borrow_mut() = args[0].get_property("name").to_js_string();
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*denied.borrow(), "NotAllowedError");

        crate::capability_gate::grant("window-management");
        crate::platform_host::set_display_override(Some(vec![
            crate::platform_host::DisplayInfo {
                label: "internal".into(),
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
                primary: true,
                internal: true,
            },
            crate::platform_host::DisplayInfo {
                label: "external".into(),
                left: 1920,
                top: 0,
                width: 1280,
                height: 800,
                primary: false,
                internal: false,
            },
        ]));
        let details = std::rc::Rc::new(std::cell::RefCell::new(Value::Undefined));
        let details_for_then = std::rc::Rc::clone(&details);
        get_screen_details().call_method(
            "then",
            vec![Value::function(move |_, args| {
                *details_for_then.borrow_mut() = args[0].clone();
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        let details = details.borrow().clone();
        assert_eq!(
            details
                .get_property("screens")
                .get_property("length")
                .to_number(),
            2.0
        );
        assert_eq!(
            details
                .get_property("screens")
                .get_property("1")
                .get_property("label")
                .to_js_string(),
            "external"
        );
        assert_eq!(
            details
                .get_property("screens")
                .get_property("1")
                .get_property("width")
                .to_number(),
            1280.0
        );
        crate::capability_gate::reset();
        crate::platform_host::reset_overrides();
    }
}
