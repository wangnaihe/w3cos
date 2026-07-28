//! Local CloseWatcher lifecycle used by dialogs, popovers and transient UI.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static CLOSE_WATCHER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn event(name: &str, cancelable: bool) -> Value {
    w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![
            Value::string(name),
            Value::object(HashMap::from([(
                "cancelable".into(),
                Value::Bool(cancelable),
            )])),
        ],
    )
}

pub fn close_watcher_class() -> Value {
    CLOSE_WATCHER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |this, args| {
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            this.set_property("oncancel", Value::Null);
            this.set_property("onclose", Value::Null);
            let active = Rc::new(Cell::new(true));

            let active_for_close = Rc::clone(&active);
            let this_for_close = this.clone();
            this.set_property(
                "__closeWatcherClose",
                realm_function(generation, move |_, _| {
                    if active_for_close.replace(false) {
                        this_for_close.call_method("dispatchEvent", vec![event("close", false)]);
                    }
                    Value::Undefined
                }),
            );

            let active_for_request = Rc::clone(&active);
            let this_for_request = this.clone();
            this.set_property(
                "__closeWatcherRequestClose",
                realm_function(generation, move |_, _| {
                    if !active_for_request.get() {
                        return Value::Undefined;
                    }
                    if this_for_request
                        .call_method("dispatchEvent", vec![event("cancel", true)])
                        .to_bool()
                    {
                        this_for_request.call_method("__closeWatcherClose", vec![]);
                    }
                    Value::Undefined
                }),
            );

            let active_for_destroy = active;
            this.set_property(
                "__closeWatcherDestroy",
                realm_function(generation, move |_, _| {
                    active_for_destroy.set(false);
                    Value::Undefined
                }),
            );

            let signal = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .get_property("signal");
            if !signal.is_undefined() {
                if signal.get_property("aborted").to_bool() {
                    this.call_method("__closeWatcherDestroy", vec![]);
                } else if signal.get_property("addEventListener").is_function() {
                    let this_for_abort = this.clone();
                    signal.call_method(
                        "addEventListener",
                        vec![
                            Value::string("abort"),
                            realm_function(generation, move |_, _| {
                                this_for_abort.call_method("__closeWatcherDestroy", vec![])
                            }),
                            Value::object(HashMap::from([("once".into(), Value::Bool(true))])),
                        ],
                    );
                }
            }
            Value::Undefined
        });
        class.set_property("name", Value::string("CloseWatcher"));
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), class.clone()),
            ("oncancel".into(), Value::Null),
            ("onclose".into(), Value::Null),
            (
                "requestClose".into(),
                realm_function(generation, |this, _| {
                    this.call_method("__closeWatcherRequestClose", vec![])
                }),
            ),
            (
                "close".into(),
                realm_function(generation, |this, _| {
                    this.call_method("__closeWatcherClose", vec![])
                }),
            ),
            (
                "destroy".into(),
                realm_function(generation, |this, _| {
                    this.call_method("__closeWatcherDestroy", vec![])
                }),
            ),
        ]));
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
    CLOSE_WATCHER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_close_is_cancelable_then_closes_once() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let watcher = w3cos_core::class::construct(&close_watcher_class(), vec![]);
        let cancels = Rc::new(Cell::new(0));
        let closes = Rc::new(Cell::new(0));
        let cancels_for_handler = Rc::clone(&cancels);
        watcher.set_property(
            "oncancel",
            Value::function(move |_, args| {
                cancels_for_handler.set(cancels_for_handler.get() + 1);
                args[0].call_method("preventDefault", vec![]);
                Value::Undefined
            }),
        );
        let closes_for_handler = Rc::clone(&closes);
        watcher.set_property(
            "onclose",
            Value::function(move |_, _| {
                closes_for_handler.set(closes_for_handler.get() + 1);
                Value::Undefined
            }),
        );
        watcher.call_method("requestClose", vec![]);
        assert_eq!(cancels.get(), 1);
        assert_eq!(closes.get(), 0);

        watcher.set_property("oncancel", Value::Null);
        watcher.call_method("requestClose", vec![]);
        watcher.call_method("close", vec![]);
        assert_eq!(closes.get(), 1);
        reset_realm();
    }

    #[test]
    fn abort_signal_destroys_without_close_event() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let controller =
            w3cos_core::class::construct(&crate::fetch::abort_controller_class(), vec![]);
        let watcher = w3cos_core::class::construct(
            &close_watcher_class(),
            vec![Value::object(HashMap::from([(
                "signal".into(),
                controller.get_property("signal"),
            )]))],
        );
        let closes = Rc::new(Cell::new(0));
        let closes_for_handler = Rc::clone(&closes);
        watcher.set_property(
            "onclose",
            Value::function(move |_, _| {
                closes_for_handler.set(closes_for_handler.get() + 1);
                Value::Undefined
            }),
        );
        controller.call_method("abort", vec![]);
        watcher.call_method("requestClose", vec![]);
        assert_eq!(closes.get(), 0);
        reset_realm();
    }

    #[test]
    fn class_and_lifecycle_callbacks_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_class = close_watcher_class();
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));
        let controller =
            w3cos_core::class::construct(&crate::fetch::abort_controller_class(), vec![]);
        let old_watcher = w3cos_core::class::construct(
            &old_class,
            vec![Value::object(HashMap::from([(
                "signal".into(),
                controller.get_property("signal"),
            )]))],
        );
        let old_events = Rc::new(Cell::new(0));
        let old_events_for_handler = Rc::clone(&old_events);
        old_watcher.set_property(
            "onclose",
            Value::function(move |_, _| {
                old_events_for_handler.set(old_events_for_handler.get() + 1);
                Value::Undefined
            }),
        );

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_class = close_watcher_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        old_watcher.call_method("requestClose", vec![]);
        old_watcher.call_method("close", vec![]);
        controller.call_method("abort", vec![]);
        assert_eq!(old_events.get(), 0);
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());

        let new_watcher = w3cos_core::class::construct(&new_class, vec![]);
        let new_events = Rc::new(Cell::new(0));
        let new_events_for_handler = Rc::clone(&new_events);
        new_watcher.set_property(
            "onclose",
            Value::function(move |_, _| {
                new_events_for_handler.set(new_events_for_handler.get() + 1);
                Value::Undefined
            }),
        );
        new_watcher.call_method("requestClose", vec![]);
        assert_eq!(new_events.get(), 1);
        reset_realm();
    }
}
