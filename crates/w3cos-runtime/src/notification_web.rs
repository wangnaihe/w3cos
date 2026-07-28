//! Browser-shaped Notifications API facade.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static NOTIFICATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn show(title: &str, body: &str, icon: &str) -> bool {
    #[cfg(all(
        any(target_os = "macos", target_os = "linux", target_os = "windows"),
        not(target_env = "ohos")
    ))]
    {
        if icon.is_empty() {
            crate::notification::show(title, body)
        } else {
            crate::notification::show_with_icon(title, body, icon)
        }
    }
    #[cfg(not(all(
        any(target_os = "macos", target_os = "linux", target_os = "windows"),
        not(target_env = "ohos")
    )))]
    {
        let _ = (title, body, icon);
        false
    }
}

pub fn notification_class() -> Value {
    NOTIFICATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |this, args| {
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            let title = args.first().cloned().unwrap_or_default().to_js_string();
            let options = args.get(1).cloned().unwrap_or_default();
            let body = options.get_property("body").to_js_string();
            let icon = options.get_property("icon").to_js_string();
            for (name, value) in [
                ("title", Value::string(&title)),
                ("body", Value::string(&body)),
                ("icon", Value::string(&icon)),
                ("tag", options.get_property("tag")),
                ("data", options.get_property("data")),
                ("onclick", Value::Null),
                ("onshow", Value::Null),
                ("onerror", Value::Null),
                ("onclose", Value::Null),
            ] {
                this.set_property(name, value);
            }
            this.set_property(
                "close",
                realm_function(generation, |this, _| {
                    let event = w3cos_core::class::construct(
                        &crate::web_events::event_class(),
                        vec![Value::string("close")],
                    );
                    this.call_method("dispatchEvent", vec![event]);
                    Value::Undefined
                }),
            );
            let event = w3cos_core::class::construct(
                &crate::web_events::event_class(),
                vec![Value::string(if show(&title, &body, &icon) {
                    "show"
                } else {
                    "error"
                })],
            );
            this.call_method("dispatchEvent", vec![event]);
            Value::Undefined
        });
        class.set_property(
            "permission",
            Value::string(
                if cfg!(all(
                    any(
                        target_os = "macos",
                        target_os = "linux",
                        target_os = "windows"
                    ),
                    not(target_env = "ohos")
                )) {
                    "granted"
                } else {
                    "denied"
                },
            ),
        );
        class.set_property("maxActions", Value::Number(2.0));
        class.set_property(
            "requestPermission",
            realm_function(generation, move |_, args| {
                let permission = notification_class().get_property("permission");
                if let Some(callback) = args.first()
                    && callback.is_function()
                {
                    callback.call(Value::Undefined, vec![permission.clone()]);
                }
                let thenable = Value::object(HashMap::new());
                thenable.set_property(
                    "then",
                    realm_function(generation, move |_, args| {
                        if let Some(callback) = args.first()
                            && callback.is_function()
                        {
                            callback.call(Value::Undefined, vec![permission.clone()]);
                        }
                        Value::Undefined
                    }),
                );
                thenable
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "actions",
            "badge",
            "body",
            "close",
            "data",
            "dir",
            "icon",
            "lang",
            "onclick",
            "onclose",
            "onerror",
            "onshow",
            "renotify",
            "requireInteraction",
            "silent",
            "tag",
            "timestamp",
            "title",
            "vibrate",
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

pub fn reset_realm() {
    NOTIFICATION_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_and_permission_callbacks_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_class = notification_class();
        let old_permission = old_class.get_property("requestPermission");
        let old_thenable = old_permission.call(Value::Undefined, vec![]);
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_class = notification_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(old_permission.call(Value::Undefined, vec![]).is_undefined());
        assert!(
            old_thenable
                .call_method("then", vec![Value::function(|_, _| Value::Undefined)])
                .is_undefined()
        );
        assert!(
            new_class
                .get_property("requestPermission")
                .call(Value::Undefined, vec![])
                .is_object()
        );
        reset_realm();
    }
}
