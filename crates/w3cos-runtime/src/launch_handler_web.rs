//! Launch Handler API queue with a platform injection entry point.

use std::cell::RefCell;
use std::collections::HashMap;

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
        let class = Value::function(move |_, _| {
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
    let prototype = launch_params_class().get_property("prototype");
    for name in ["files", "targetURL"] {
        prototype.set_property(name, value.get_property(name));
    }
    w3cos_core::class::set_prototype_of(&value, &prototype);
    value
}

fn schedule_delivery(consumer: Value, params: Value) {
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        consumer.call(Value::Undefined, vec![params.clone()]);
        Value::Undefined
    }));
}

/// Queue a native/PWA launch. Launches arriving before `setConsumer()` are
/// retained and delivered in order once application code installs a consumer.
pub fn enqueue_launch(files: Vec<Value>, target_url: &str) {
    let params = launch_params(files, target_url);
    let consumer = CONSUMER.with(|consumer| consumer.borrow().clone());
    if let Some(consumer) = consumer {
        schedule_delivery(consumer, params);
    } else {
        PENDING.with(|pending| pending.borrow_mut().push(params));
    }
}

pub fn launch_queue_value() -> Value {
    LAUNCH_QUEUE.with(|slot| {
        if let Some(queue) = slot.borrow().clone() {
            return queue;
        }
        let queue = Value::object(HashMap::new());
        queue.set_property(
            "setConsumer",
            Value::function(|_, args| {
                let consumer = args.first().cloned().unwrap_or(Value::Undefined);
                if !consumer.is_function() {
                    w3cos_core::throw_value(type_error(
                        "LaunchQueue.setConsumer requires a callable consumer",
                    ));
                }
                CONSUMER.with(|slot| *slot.borrow_mut() = Some(consumer.clone()));
                let pending = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
                for params in pending {
                    schedule_delivery(consumer.clone(), params);
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn launch_before_consumer_is_delivered_asynchronously() {
        CONSUMER.with(|consumer| *consumer.borrow_mut() = None);
        PENDING.with(|pending| pending.borrow_mut().clear());
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
    }
}
