use crate::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

type HostModule = Rc<dyn Fn(&str, Vec<Value>) -> Value>;

thread_local! {
    static HOST_MODULES: RefCell<HashMap<String, HostModule>> = RefCell::new(HashMap::new());
}

/// Register an application-owned native module for the current UI thread.
///
/// W3COS only owns routing. Module names, operations, state and permissions are
/// defined by the embedding application.
pub fn register_host_module(
    module: impl Into<String>,
    handler: impl Fn(&str, Vec<Value>) -> Value + 'static,
) {
    HOST_MODULES.with(|modules| {
        modules.borrow_mut().insert(module.into(), Rc::new(handler));
    });
}

pub fn unregister_host_module(module: &str) {
    HOST_MODULES.with(|modules| {
        modules.borrow_mut().remove(module);
    });
}

pub fn invoke(arguments: Vec<Value>) -> Value {
    let module = arguments
        .first()
        .map(Value::to_js_string)
        .unwrap_or_default();
    let operation = arguments
        .get(1)
        .map(Value::to_js_string)
        .unwrap_or_default();
    let payload = arguments.into_iter().skip(2).collect::<Vec<_>>();
    HOST_MODULES.with(|modules| {
        let handler = modules.borrow().get(&module).cloned();
        handler
            .map(|handler| handler(&operation, payload))
            .unwrap_or(Value::Undefined)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_by_module_and_operation() {
        register_host_module("test.echo", |operation, arguments| {
            Value::from(format!(
                "{operation}:{}",
                arguments
                    .first()
                    .map(Value::to_js_string)
                    .unwrap_or_default()
            ))
        });
        assert_eq!(
            invoke(vec![
                Value::from("test.echo"),
                Value::from("send"),
                Value::from("hello"),
            ])
            .to_js_string(),
            "send:hello"
        );
        unregister_host_module("test.echo");
        assert!(invoke(vec![Value::from("test.echo")]).is_undefined());
    }
}
