//! Navigator identities, legacy plugin collections, and protocol handlers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static PROTOCOL_HANDLERS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static MANAGED_CONFIGURATION: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static MANAGED_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static LOGIN_STATUS: RefCell<String> = const { RefCell::new(String::new()) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn illegal_class(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        let members: &[&str] = match name {
            "Plugin" => &[
                "length",
                "name",
                "filename",
                "description",
                "item",
                "namedItem",
            ],
            "PluginArray" => &["length", "item", "namedItem", "refresh"],
            "MimeType" => &["type", "suffixes", "description", "enabledPlugin"],
            "MimeTypeArray" => &["length", "item", "namedItem"],
            "NavigatorLogin" => &["setStatus"],
            "NavigatorManagedData" => &[
                "getManagedConfiguration",
                "onmanagedconfigurationchange",
            ],
            _ => &[],
        };
        for member in members {
            prototype.set_property(member, Value::Undefined);
        }
        if name == "Navigator" {
            for member in "adAuctionComponents appCodeName appName appVersion bluetooth \
                canLoadAdAuctionFencedFrame canShare clearAppBadge clearOriginJoinedAdInterestGroups \
                clipboard connection cookieEnabled createAuctionNonce credentials \
                deprecatedReplaceInURN deprecatedRunAdAuctionEnforcesKAnonymity deprecatedURNToURL \
                deviceMemory devicePosture doNotTrack geolocation getBattery getGamepads \
                getInstalledRelatedApps getInterestGroupAdAuctionData getUserMedia gpu \
                hardwareConcurrency hid ink javaEnabled joinAdInterestGroup keyboard language \
                languages leaveAdInterestGroup locks login managed maxTouchPoints mediaCapabilities \
                mediaDevices mediaSession mimeTypes onLine pdfViewerEnabled permissions platform \
                plugins presentation product productSub protectedAudience registerProtocolHandler \
                requestMIDIAccess requestMediaKeySystemAccess runAdAuction scheduling sendBeacon \
                serial serviceWorker setAppBadge share storage storageBuckets \
                unregisterProtocolHandler updateAdInterestGroups usb userActivation userAgent \
                userAgentData vendor vendorSub vibrate virtualKeyboard wakeLock webdriver \
                webkitGetUserMedia webkitPersistentStorage webkitTemporaryStorage \
                windowControlsOverlay xr"
                .split_whitespace()
            {
                prototype.set_property(member, Value::Undefined);
            }
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn navigator_class() -> Value {
    illegal_class("Navigator")
}

pub fn plugin_class() -> Value {
    illegal_class("Plugin")
}

pub fn plugin_array_class() -> Value {
    illegal_class("PluginArray")
}

pub fn mime_type_class() -> Value {
    illegal_class("MimeType")
}

pub fn mime_type_array_class() -> Value {
    illegal_class("MimeTypeArray")
}

pub fn navigator_login_class() -> Value {
    illegal_class("NavigatorLogin")
}

pub fn navigator_managed_data_class() -> Value {
    let class = illegal_class("NavigatorManagedData");
    w3cos_core::class::set_prototype_of(
        &class.get_property("prototype"),
        &crate::web_events::event_target_class().get_property("prototype"),
    );
    class
}

pub fn navigator_login_value() -> Value {
    let value = Value::object(HashMap::from([(
        "setStatus".into(),
        Value::function(|_, args| {
            let status = args.first().cloned().unwrap_or_default().to_js_string();
            if !matches!(status.as_str(), "logged-in" | "logged-out") {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    "login status must be 'logged-in' or 'logged-out'",
                )]);
            }
            LOGIN_STATUS.with(|current| *current.borrow_mut() = status);
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: navigator.login stores status in this runtime; browser \
                     identity-provider integration requires a host account adapter"
                );
            });
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    )]));
    w3cos_core::class::set_prototype_of(&value, &navigator_login_class().get_property("prototype"));
    value
}

pub fn navigator_managed_data_value() -> Value {
    MANAGED_VALUE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
        value.set_property("onmanagedconfigurationchange", Value::Null);
        value.set_property(
            "getManagedConfiguration",
            Value::function(|_, args| {
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: navigator.managed returns host-injected configuration; \
                         operating-system enterprise policy discovery requires a host adapter"
                    );
                });
                let keys = args.first().cloned().unwrap_or_default().iter();
                let result = MANAGED_CONFIGURATION.with(|configuration| {
                    let configuration = configuration.borrow();
                    Value::object(
                        keys.into_iter()
                            .filter_map(|key| {
                                let key = key.to_js_string();
                                configuration.get(&key).cloned().map(|value| (key, value))
                            })
                            .collect(),
                    )
                });
                w3cos_core::promise::resolve(vec![result])
            }),
        );
        w3cos_core::class::set_prototype_of(
            &value,
            &navigator_managed_data_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn update_managed_configuration(configuration: HashMap<String, Value>) {
    MANAGED_CONFIGURATION.with(|current| *current.borrow_mut() = configuration);
    if let Some(value) = MANAGED_VALUE.with(|slot| slot.borrow().clone()) {
        let event = w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![Value::string("managedconfigurationchange")],
        );
        value.call_method("dispatchEvent", vec![event]);
    }
}

pub fn login_status() -> String {
    LOGIN_STATUS.with(|status| status.borrow().clone())
}

fn empty_legacy_array(class: Value, refresh: bool) -> Value {
    let value = Value::object(HashMap::from([
        ("length".into(), Value::Number(0.0)),
        ("item".into(), Value::function(|_, _| Value::Null)),
        ("namedItem".into(), Value::function(|_, _| Value::Null)),
    ]));
    if refresh {
        value.set_property("refresh", Value::function(|_, _| Value::Undefined));
    }
    w3cos_core::class::set_prototype_of(&value, &class.get_property("prototype"));
    value
}

pub fn plugin_array_value() -> Value {
    empty_legacy_array(plugin_array_class(), true)
}

pub fn mime_type_array_value() -> Value {
    empty_legacy_array(mime_type_array_class(), false)
}

fn valid_scheme(scheme: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "bitcoin",
        "ftp",
        "ftps",
        "geo",
        "im",
        "irc",
        "ircs",
        "magnet",
        "mailto",
        "matrix",
        "mms",
        "news",
        "nntp",
        "openpgp4fpr",
        "sip",
        "sms",
        "smsto",
        "ssh",
        "tel",
        "urn",
        "webcal",
        "wtai",
        "xmpp",
    ];
    ALLOWED.contains(&scheme)
        || scheme.strip_prefix("web+").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_lowercase())
        })
}

pub fn register_protocol_handler_value() -> Value {
    Value::function(|_, args| {
        let scheme = args
            .first()
            .cloned()
            .unwrap_or(Value::Undefined)
            .to_js_string()
            .to_ascii_lowercase();
        let url = args
            .get(1)
            .cloned()
            .unwrap_or(Value::Undefined)
            .to_js_string();
        if !valid_scheme(&scheme) {
            w3cos_core::throw_value(error(
                "SecurityError",
                "protocol scheme is not allowed for custom handlers",
            ));
        }
        if !(url.starts_with("https://")
            || url.starts_with("http://")
            || url.starts_with("w3cos://"))
            || !url.contains("%s")
        {
            w3cos_core::throw_value(error(
                "SyntaxError",
                "protocol handler URL must be an allowed URL containing %s",
            ));
        }
        PROTOCOL_HANDLERS.with(|handlers| {
            handlers.borrow_mut().insert(scheme, url);
        });
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: registerProtocolHandler records the validated handler in this \
                 runtime; OS-level protocol association and user consent require a platform adapter"
            );
        });
        Value::Undefined
    })
}

pub fn registered_handler(scheme: &str) -> Option<String> {
    PROTOCOL_HANDLERS.with(|handlers| handlers.borrow().get(scheme).cloned())
}

pub fn reset() {
    PROTOCOL_HANDLERS.with(|handlers| handlers.borrow_mut().clear());
    MANAGED_CONFIGURATION.with(|configuration| configuration.borrow_mut().clear());
    MANAGED_VALUE.with(|value| *value.borrow_mut() = None);
    LOGIN_STATUS.with(|status| status.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn empty_plugin_collections_and_protocol_registration_are_standard_shaped() {
        let plugins = plugin_array_value();
        assert!(w3cos_core::class::instance_of(
            &plugins,
            &plugin_array_class()
        ));
        assert_eq!(plugins.get_property("length").to_u32(), 0);
        assert!(
            plugins
                .call_method("item", vec![Value::Number(0.0)])
                .is_null()
        );
        assert!(
            plugins
                .call_method("namedItem", vec![Value::string("missing")])
                .is_null()
        );

        reset();
        register_protocol_handler_value().call(
            Value::Undefined,
            vec![
                Value::string("web+wcos"),
                Value::string("https://example.test/open?url=%s"),
            ],
        );
        assert_eq!(
            registered_handler("web+wcos").as_deref(),
            Some("https://example.test/open?url=%s")
        );
    }

    #[test]
    fn managed_configuration_and_login_status_are_host_aware() {
        reset();
        let managed = navigator_managed_data_value();
        let changes = Rc::new(Cell::new(0));
        let changes_for_listener = Rc::clone(&changes);
        managed.call_method(
            "addEventListener",
            vec![
                Value::string("managedconfigurationchange"),
                Value::function(move |_, _| {
                    changes_for_listener.set(changes_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        update_managed_configuration(HashMap::from([
            ("environment".into(), Value::string("production")),
            ("hidden".into(), Value::string("not-requested")),
        ]));
        assert_eq!(changes.get(), 1);
        let result = Rc::new(RefCell::new(Value::Undefined));
        let result_for_callback = Rc::clone(&result);
        managed
            .call_method(
                "getManagedConfiguration",
                vec![Value::array(vec![Value::string("environment")])],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *result_for_callback.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            result.borrow().get_property("environment").to_js_string(),
            "production"
        );
        assert!(result.borrow().get_property("hidden").is_undefined());

        navigator_login_value()
            .call_method("setStatus", vec![Value::string("logged-in")])
            .call_method("then", vec![Value::function(|_, _| Value::Undefined)]);
        w3cos_core::promise::drain_microtasks();
        assert_eq!(login_status(), "logged-in");
    }
}
