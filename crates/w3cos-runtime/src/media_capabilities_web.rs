//! Media Capabilities API backed by a host-registerable codec table.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityDirection {
    Decode,
    Encode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityInfo {
    pub smooth: bool,
    pub power_efficient: bool,
}

thread_local! {
    static DECODE_CAPABILITIES: RefCell<HashMap<String, CapabilityInfo>> = RefCell::new(HashMap::new());
    static ENCODE_CAPABILITIES: RefCell<HashMap<String, CapabilityInfo>> = RefCell::new(HashMap::new());
    static MEDIA_CAPABILITIES_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn normalized_mime(content_type: &str) -> Option<String> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let (kind, subtype) = mime.split_once('/')?;
    (!kind.is_empty() && !subtype.is_empty()).then_some(mime)
}

fn configuration_mimes(configuration: &Value) -> Result<Vec<String>, Value> {
    if !configuration.is_object() {
        return Err(error(
            "TypeError",
            "media capability query requires a configuration object",
        ));
    }
    let mut mimes = Vec::new();
    for name in ["audio", "video"] {
        let media = configuration.get_property(name);
        if media.is_undefined() {
            continue;
        }
        if !media.is_object() {
            return Err(error(
                "TypeError",
                &format!("{name} media configuration must be an object"),
            ));
        }
        let content_type = media.get_property("contentType").to_js_string();
        let Some(mime) = normalized_mime(&content_type) else {
            return Err(error(
                "TypeError",
                &format!("{name}.contentType must contain a valid MIME type"),
            ));
        };
        mimes.push(mime);
    }
    if mimes.is_empty() {
        return Err(error(
            "TypeError",
            "media capability configuration requires audio or video",
        ));
    }
    Ok(mimes)
}

fn valid_type(direction: CapabilityDirection, value: &str) -> bool {
    match direction {
        CapabilityDirection::Decode => matches!(value, "file" | "media-source" | "webrtc"),
        CapabilityDirection::Encode => matches!(value, "record" | "transmission" | "webrtc"),
    }
}

fn query(configuration: Value, direction: CapabilityDirection) -> Value {
    let type_name = configuration.get_property("type").to_js_string();
    if !valid_type(direction, &type_name) {
        return w3cos_core::promise::reject(vec![error(
            "TypeError",
            "unsupported MediaCapabilities configuration type",
        )]);
    }
    let mimes = match configuration_mimes(&configuration) {
        Ok(mimes) => mimes,
        Err(error) => return w3cos_core::promise::reject(vec![error]),
    };
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: MediaCapabilities uses the registered host codec table; \
             unregistered formats return supported=false and DRM key-system access remains unavailable"
        );
    });
    let capabilities = match direction {
        CapabilityDirection::Decode => {
            DECODE_CAPABILITIES.with(|capabilities| capabilities.borrow().clone())
        }
        CapabilityDirection::Encode => {
            ENCODE_CAPABILITIES.with(|capabilities| capabilities.borrow().clone())
        }
    };
    let infos: Vec<CapabilityInfo> = mimes
        .iter()
        .filter_map(|mime| capabilities.get(mime).copied())
        .collect();
    let supported = infos.len() == mimes.len();
    let smooth = supported && infos.iter().all(|info| info.smooth);
    let power_efficient = supported && infos.iter().all(|info| info.power_efficient);
    let result = Value::object(HashMap::from([
        ("supported".into(), Value::Bool(supported)),
        ("smooth".into(), Value::Bool(smooth)),
        ("powerEfficient".into(), Value::Bool(power_efficient)),
    ]));
    if direction == CapabilityDirection::Decode {
        result.set_property("keySystemAccess", Value::Null);
    }
    w3cos_core::promise::resolve(vec![result])
}

pub fn media_capabilities_class() -> Value {
    MEDIA_CAPABILITIES_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: MediaCapabilities"))
        });
        class.set_property("name", Value::string("MediaCapabilities"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["decodingInfo", "encodingInfo"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn media_capabilities_value() -> Value {
    let capabilities = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(
        &capabilities,
        &media_capabilities_class().get_property("prototype"),
    );
    capabilities.set_property(
        "decodingInfo",
        Value::function(|_, args| {
            query(
                args.first().cloned().unwrap_or(Value::Undefined),
                CapabilityDirection::Decode,
            )
        }),
    );
    capabilities.set_property(
        "encodingInfo",
        Value::function(|_, args| {
            query(
                args.first().cloned().unwrap_or(Value::Undefined),
                CapabilityDirection::Encode,
            )
        }),
    );
    capabilities
}

/// Register one codec capability supplied by a platform media adapter.
pub fn register_capability(
    direction: CapabilityDirection,
    content_type: &str,
    info: CapabilityInfo,
) -> bool {
    let Some(mime) = normalized_mime(content_type) else {
        return false;
    };
    match direction {
        CapabilityDirection::Decode => {
            DECODE_CAPABILITIES.with(|capabilities| capabilities.borrow_mut().insert(mime, info));
        }
        CapabilityDirection::Encode => {
            ENCODE_CAPABILITIES.with(|capabilities| capabilities.borrow_mut().insert(mime, info));
        }
    }
    true
}

pub fn reset() {
    DECODE_CAPABILITIES.with(|capabilities| capabilities.borrow_mut().clear());
    ENCODE_CAPABILITIES.with(|capabilities| capabilities.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn configuration(type_name: &str, content_type: &str) -> Value {
        Value::object(HashMap::from([
            ("type".into(), Value::string(type_name)),
            (
                "audio".into(),
                Value::object(HashMap::from([(
                    "contentType".into(),
                    Value::string(content_type),
                )])),
            ),
        ]))
    }

    #[test]
    fn registered_codecs_report_traits_and_unknown_formats_stay_false() {
        reset();
        assert!(register_capability(
            CapabilityDirection::Decode,
            "audio/mpeg",
            CapabilityInfo {
                smooth: true,
                power_efficient: false,
            }
        ));
        let capabilities = media_capabilities_value();
        assert!(w3cos_core::class::instance_of(
            &capabilities,
            &media_capabilities_class()
        ));
        let results = Rc::new(RefCell::new(Vec::<String>::new()));
        for configuration in [
            configuration("file", "audio/mpeg; codecs=mp3"),
            configuration("file", "audio/unknown"),
        ] {
            let results_for_then = Rc::clone(&results);
            capabilities
                .call_method("decodingInfo", vec![configuration])
                .call_method(
                    "then",
                    vec![Value::function(move |_, args| {
                        let info = args[0].clone();
                        results_for_then.borrow_mut().push(format!(
                            "{}:{}:{}:{}",
                            info.get_property("supported").to_js_string(),
                            info.get_property("smooth").to_js_string(),
                            info.get_property("powerEfficient").to_js_string(),
                            info.get_property("keySystemAccess").is_null()
                        ));
                        Value::Undefined
                    })],
                );
        }
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            &*results.borrow(),
            &["true:true:false:true", "false:false:false:true"]
        );
    }
}
