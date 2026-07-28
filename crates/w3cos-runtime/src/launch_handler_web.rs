//! Launch Handler API queue with a platform injection entry point.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static LAUNCH_QUEUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CONSUMER: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PENDING: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

fn type_error(message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string("TypeError")],
    )
}

fn illegal_class(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |_, _| {
            w3cos_core::throw_value(type_error(&format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn launch_queue_class() -> Value {
    illegal_class("LaunchQueue")
}

pub fn launch_params_class() -> Value {
    let class = illegal_class("LaunchParams");
    for property in ["files", "targetURL"] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}

fn launch_params(files: Vec<Value>, target_url: &str) -> Value {
    let value = Value::object(HashMap::from([
        ("files".into(), Value::array(files)),
        ("targetURL".into(), Value::string(target_url)),
    ]));
    w3cos_core::class::set_prototype_of(&value, &launch_params_class().get_property("prototype"));
    value
}

fn schedule_delivery(generation: u32, consumer: Value, params: Value) {
    crate::jsdom::queue_microtask_value(realm_function(generation, move |_, _| {
        consumer.call(Value::Undefined, vec![params.clone()]);
        Value::Undefined
    }));
}

/// Queue a native/PWA launch. Launches arriving before `setConsumer()` are
/// retained and delivered in order once application code installs a consumer.
pub fn enqueue_launch(files: Vec<Value>, target_url: &str) {
    let generation = crate::jsdom::realm_generation();
    let params = launch_params(files, target_url);
    let consumer = CONSUMER.with(|consumer| consumer.borrow().clone());
    if let Some(consumer) = consumer {
        schedule_delivery(generation, consumer, params);
    } else {
        PENDING.with(|pending| pending.borrow_mut().push(params));
    }
}

pub fn launch_queue_value() -> Value {
    LAUNCH_QUEUE.with(|slot| {
        if let Some(queue) = slot.borrow().clone() {
            return queue;
        }
        let generation = crate::jsdom::realm_generation();
        let queue = Value::object(HashMap::new());
        queue.set_property(
            "setConsumer",
            realm_function(generation, move |_, args| {
                let consumer = args.first().cloned().unwrap_or(Value::Undefined);
                if !consumer.is_function() {
                    w3cos_core::throw_value(type_error(
                        "LaunchQueue.setConsumer requires a callable consumer",
                    ));
                }
                CONSUMER.with(|slot| *slot.borrow_mut() = Some(consumer.clone()));
                let pending = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
                for params in pending {
                    schedule_delivery(generation, consumer.clone(), params);
                }
                Value::Undefined
            }),
        );
        let prototype = launch_queue_class().get_property("prototype");
        prototype.set_property("setConsumer", queue.get_property("setConsumer"));
        w3cos_core::class::set_prototype_of(&queue, &prototype);
        *slot.borrow_mut() = Some(queue.clone());
        queue
    })
}

pub fn reset_realm() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
    LAUNCH_QUEUE.with(|queue| {
        queue.borrow_mut().take();
    });
    CONSUMER.with(|consumer| {
        consumer.borrow_mut().take();
    });
    PENDING.with(|pending| pending.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn launch_before_consumer_is_delivered_asynchronously() {
        reset_realm();
        enqueue_launch(vec![Value::string("handle")], "w3cos://app/open");
        let received = Rc::new(RefCell::new(String::new()));
        let received_for_consumer = Rc::clone(&received);
        launch_queue_value().call_method(
            "setConsumer",
            vec![Value::function(move |_, args| {
                *received_for_consumer.borrow_mut() =
                    args[0].get_property("targetURL").to_js_string();
                Value::Undefined
            })],
        );
        assert!(received.borrow().is_empty());
        crate::jsdom::drain_microtasks();
        assert_eq!(&*received.borrow(), "w3cos://app/open");
        reset_realm();
    }

    #[test]
    fn launch_params_keep_instance_fields_off_the_shared_prototype() {
        reset_realm();
        let first = launch_params(vec![Value::string("first")], "w3cos://app/first");
        let second = launch_params(vec![Value::string("second")], "w3cos://app/second");

        assert_eq!(
            first.get_property("targetURL").to_js_string(),
            "w3cos://app/first"
        );
        assert_eq!(
            second.get_property("targetURL").to_js_string(),
            "w3cos://app/second"
        );
        let prototype = launch_params_class().get_property("prototype");
        assert!(prototype.get_property("targetURL").is_undefined());
        assert!(prototype.get_property("files").is_undefined());
        reset_realm();
    }

    #[test]
    fn queue_consumers_and_pending_launches_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_queue = launch_queue_value();
        let old_queue_class = launch_queue_class();
        let old_params_class = launch_params_class();
        old_queue_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));
        let stale_deliveries = Rc::new(Cell::new(0));
        old_queue.call_method(
            "setConsumer",
            vec![Value::function({
                let stale_deliveries = Rc::clone(&stale_deliveries);
                move |_, _| {
                    stale_deliveries.set(stale_deliveries.get() + 1);
                    Value::Undefined
                }
            })],
        );
        enqueue_launch(vec![], "w3cos://app/stale");

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::drain_microtasks();
        assert_eq!(stale_deliveries.get(), 0);

        let new_queue = launch_queue_value();
        let new_queue_class = launch_queue_class();
        let new_params_class = launch_params_class();
        assert!(old_queue != new_queue);
        assert!(old_queue_class != new_queue_class);
        assert!(old_params_class != new_params_class);
        assert!(
            new_queue_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(
            old_queue
                .call_method(
                    "setConsumer",
                    vec![Value::function(|_, _| Value::Undefined)]
                )
                .is_undefined()
        );

        enqueue_launch(vec![Value::string("fresh")], "w3cos://app/fresh");
        let received = Rc::new(RefCell::new(String::new()));
        new_queue.call_method(
            "setConsumer",
            vec![Value::function({
                let received = Rc::clone(&received);
                move |_, args| {
                    *received.borrow_mut() = args[0].get_property("targetURL").to_js_string();
                    Value::Undefined
                }
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*received.borrow(), "w3cos://app/fresh");
        reset_realm();
    }
}
