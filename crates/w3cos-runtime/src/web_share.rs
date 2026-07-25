//! Web Share API validation and host-compatible fallback.

use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn valid_url(value: &Value) -> bool {
    if value.is_undefined() {
        return true;
    }
    let value = value.to_js_string();
    value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("mailto:")
        || value.starts_with("w3cos://")
}

fn valid_files(value: &Value) -> bool {
    if value.is_undefined() {
        return true;
    }
    let files: Vec<Value> = value.iter().collect();
    !files.is_empty()
        && files
            .iter()
            .all(|file| w3cos_core::class::instance_of(file, &crate::files::file_class()))
}

pub fn can_share(data: &Value) -> bool {
    if !data.is_object() {
        return false;
    }
    let title = data.get_property("title");
    let text = data.get_property("text");
    let url = data.get_property("url");
    let files = data.get_property("files");
    let has_member = !title.is_undefined()
        || !text.is_undefined()
        || !url.is_undefined()
        || !files.is_undefined();
    has_member && valid_url(&url) && valid_files(&files)
}

pub fn can_share_value() -> Value {
    Value::function(|_, args| {
        Value::Bool(can_share(
            &args.first().cloned().unwrap_or(Value::Undefined),
        ))
    })
}

pub fn share_value() -> Value {
    Value::function(|_, args| {
        let data = args.first().cloned().unwrap_or(Value::Undefined);
        if !can_share(&data) {
            return w3cos_core::promise::reject(vec![error(
                "TypeError",
                "navigator.share requires valid title, text, url, or File data",
            )]);
        }
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: navigator.share validated the payload but no native share \
                 sheet/user-activation adapter is available; rejecting with NotAllowedError"
            );
        });
        w3cos_core::promise::reject(vec![error(
            "NotAllowedError",
            "native sharing requires transient user activation and a host share adapter",
        )])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn validates_share_data_and_rejects_with_compatible_errors() {
        assert!(!can_share(&Value::object(HashMap::new())));
        assert!(!can_share(&Value::object(HashMap::from([(
            "url".into(),
            Value::string("javascript:bad()"),
        )]))));
        assert!(can_share(&Value::object(HashMap::from([(
            "text".into(),
            Value::string("hello"),
        )]))));

        let names = Rc::new(RefCell::new(Vec::<String>::new()));
        for data in [
            Value::object(HashMap::new()),
            Value::object(HashMap::from([(
                "url".into(),
                Value::string("https://example.test/share"),
            )])),
        ] {
            let names_for_catch = Rc::clone(&names);
            share_value()
                .call(Value::Undefined, vec![data])
                .call_method(
                    "catch",
                    vec![Value::function(move |_, args| {
                        names_for_catch
                            .borrow_mut()
                            .push(args[0].get_property("name").to_js_string());
                        Value::Undefined
                    })],
                );
        }
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            *names.borrow(),
            vec!["TypeError".to_string(), "NotAllowedError".to_string()]
        );
    }
}
