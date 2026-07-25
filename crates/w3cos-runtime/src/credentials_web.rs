//! Credential Management API with explicit secure-vault boundaries.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CREDENTIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PASSWORD_CREDENTIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FEDERATED_CREDENTIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CREDENTIALS_CONTAINER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static AUTHENTICATOR_RESPONSE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static AUTHENTICATOR_ASSERTION_RESPONSE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static AUTHENTICATOR_ATTESTATION_RESPONSE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PUBLIC_KEY_CREDENTIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static OTP_CREDENTIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DIGITAL_CREDENTIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IDENTITY_CREDENTIAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IDENTITY_CREDENTIAL_ERROR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IDENTITY_PROVIDER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn required_string(init: &Value, name: &str) -> String {
    let value = init.get_property(name);
    if value.is_undefined() || value.is_null() {
        w3cos_core::throw_value(error(
            "TypeError",
            &format!("credential field {name} is required"),
        ));
    }
    let value = value.to_js_string();
    if value.is_empty() {
        w3cos_core::throw_value(error(
            "TypeError",
            &format!("credential field {name} must not be empty"),
        ));
    }
    value
}

fn optional_string(init: &Value, name: &str) -> Value {
    let value = init.get_property(name);
    let text = if value.is_undefined() || value.is_null() {
        String::new()
    } else {
        value.to_js_string()
    };
    Value::string(&text)
}

fn warn_secure_store() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: Credential Management object creation is available, but credential \
             retrieval/persistence requires a user-consent and secure-vault platform adapter"
        );
    });
}

fn unavailable_identity_operation(operation: &str) -> Value {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: digital and federated identity credentials expose browser-compatible \
             interfaces; wallet, identity-provider and user-consent flows require host adapters"
        );
    });
    w3cos_core::promise::reject(vec![error(
        "NotSupportedError",
        &format!("{operation} requires a digital identity provider adapter"),
    )])
}

pub fn credential_class() -> Value {
    CREDENTIAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: Credential"))
        });
        class.set_property("name", Value::string("Credential"));
        class.set_property(
            "isConditionalMediationAvailable",
            Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Bool(false)])),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("id", Value::Undefined);
        prototype.set_property("type", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn password_credential_class() -> Value {
    PASSWORD_CREDENTIAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            if !init.is_object() {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "PasswordCredential requires a credential data object",
                ));
            }
            this.set_property("id", Value::string(&required_string(&init, "id")));
            this.set_property(
                "password",
                Value::string(&required_string(&init, "password")),
            );
            this.set_property("name", optional_string(&init, "name"));
            this.set_property("iconURL", optional_string(&init, "iconURL"));
            this.set_property("type", Value::string("password"));
            Value::Undefined
        });
        class.set_property("name", Value::string("PasswordCredential"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["iconURL", "name", "password"] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &credential_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn federated_credential_class() -> Value {
    FEDERATED_CREDENTIAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            if !init.is_object() {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "FederatedCredential requires a credential data object",
                ));
            }
            this.set_property("id", Value::string(&required_string(&init, "id")));
            this.set_property(
                "provider",
                Value::string(&required_string(&init, "provider")),
            );
            this.set_property("protocol", optional_string(&init, "protocol"));
            this.set_property("name", optional_string(&init, "name"));
            this.set_property("iconURL", optional_string(&init, "iconURL"));
            this.set_property("type", Value::string("federated"));
            Value::Undefined
        });
        class.set_property("name", Value::string("FederatedCredential"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["iconURL", "name", "protocol", "provider"] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &credential_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn credentials_container_class() -> Value {
    CREDENTIALS_CONTAINER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error(
                "TypeError",
                "Illegal constructor: CredentialsContainer",
            ))
        });
        class.set_property("name", Value::string("CredentialsContainer"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["create", "get", "preventSilentAccess", "store"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn illegal_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    members: &'static [&'static str],
    parent: Option<Value>,
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in members {
            prototype.set_property(member, Value::Undefined);
        }
        if let Some(parent) = parent {
            w3cos_core::class::set_prototype_of(&prototype, &parent);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn authenticator_response_class() -> Value {
    illegal_class(
        &AUTHENTICATOR_RESPONSE_CLASS,
        "AuthenticatorResponse",
        &["clientDataJSON"],
        None,
    )
}

pub fn authenticator_assertion_response_class() -> Value {
    illegal_class(
        &AUTHENTICATOR_ASSERTION_RESPONSE_CLASS,
        "AuthenticatorAssertionResponse",
        &["authenticatorData", "signature", "userHandle"],
        Some(authenticator_response_class().get_property("prototype")),
    )
}

pub fn authenticator_attestation_response_class() -> Value {
    illegal_class(
        &AUTHENTICATOR_ATTESTATION_RESPONSE_CLASS,
        "AuthenticatorAttestationResponse",
        &[
            "attestationObject",
            "getAuthenticatorData",
            "getPublicKey",
            "getPublicKeyAlgorithm",
            "getTransports",
        ],
        Some(authenticator_response_class().get_property("prototype")),
    )
}

pub fn otp_credential_class() -> Value {
    illegal_class(
        &OTP_CREDENTIAL_CLASS,
        "OTPCredential",
        &["code"],
        Some(credential_class().get_property("prototype")),
    )
}

pub fn public_key_credential_class() -> Value {
    PUBLIC_KEY_CREDENTIAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class(
            &PUBLIC_KEY_CREDENTIAL_CLASS,
            "PublicKeyCredential",
            &[
                "authenticatorAttachment",
                "getClientExtensionResults",
                "rawId",
                "response",
                "toJSON",
            ],
            Some(credential_class().get_property("prototype")),
        );
        for method in [
            "isConditionalMediationAvailable",
            "isUserVerifyingPlatformAuthenticatorAvailable",
        ] {
            class.set_property(
                method,
                Value::function(|_, _| {
                    warn_secure_store();
                    w3cos_core::promise::resolve(vec![Value::Bool(false)])
                }),
            );
        }
        class.set_property(
            "getClientCapabilities",
            Value::function(|_, _| {
                warn_secure_store();
                w3cos_core::promise::resolve(vec![Value::object(HashMap::new())])
            }),
        );
        for method in [
            "parseCreationOptionsFromJSON",
            "parseRequestOptionsFromJSON",
        ] {
            class.set_property(
                method,
                Value::function(|_, args| args.first().cloned().unwrap_or(Value::Undefined)),
            );
        }
        for method in [
            "signalAllAcceptedCredentials",
            "signalCurrentUserDetails",
            "signalUnknownCredential",
        ] {
            class.set_property(
                method,
                Value::function(|_, _| {
                    warn_secure_store();
                    w3cos_core::promise::resolve(vec![Value::Undefined])
                }),
            );
        }
        class
    })
}

pub fn digital_credential_class() -> Value {
    DIGITAL_CREDENTIAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class(
            &DIGITAL_CREDENTIAL_CLASS,
            "DigitalCredential",
            &["data", "protocol", "toJSON"],
            Some(credential_class().get_property("prototype")),
        );
        class.set_property(
            "userAgentAllowsProtocol",
            Value::function(|_, args| {
                let protocol = args.first().cloned().unwrap_or_default().to_js_string();
                if protocol.trim().is_empty() {
                    return w3cos_core::promise::reject(vec![error(
                        "TypeError",
                        "A digital credential protocol is required",
                    )]);
                }
                warn_secure_store();
                w3cos_core::promise::resolve(vec![Value::Bool(false)])
            }),
        );
        class
    })
}

pub fn identity_credential_class() -> Value {
    IDENTITY_CREDENTIAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class(
            &IDENTITY_CREDENTIAL_CLASS,
            "IdentityCredential",
            &["configURL", "isAutoSelected", "token"],
            Some(credential_class().get_property("prototype")),
        );
        class.set_property(
            "disconnect",
            Value::function(|_, _| unavailable_identity_operation("IdentityCredential.disconnect")),
        );
        class
    })
}

pub fn identity_credential_error_class() -> Value {
    IDENTITY_CREDENTIAL_ERROR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let message = args.first().map(Value::to_js_string).unwrap_or_default();
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            crate::unsupported::dom_exception_class().call(
                this.clone(),
                vec![
                    Value::string(&message),
                    Value::string("IdentityCredentialError"),
                ],
            );
            this.set_property("code", optional_string(&options, "code"));
            let nested = options.get_property("error");
            this.set_property(
                "error",
                if nested.is_undefined() {
                    Value::Null
                } else {
                    nested
                },
            );
            this.set_property("url", optional_string(&options, "url"));
            Value::Undefined
        });
        class.set_property("name", Value::string("IdentityCredentialError"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ["code", "error", "url"] {
            prototype.set_property(member, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::unsupported::dom_exception_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn identity_provider_class() -> Value {
    IDENTITY_PROVIDER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: IdentityProvider"))
        });
        class.set_property("name", Value::string("IdentityProvider"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        for method in ["close", "getUserInfo", "resolve"] {
            class.set_property(
                method,
                Value::function(move |_, _| {
                    unavailable_identity_operation(&format!("IdentityProvider.{method}"))
                }),
            );
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn credentials_container_value() -> Value {
    let container = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(
        &container,
        &credentials_container_class().get_property("prototype"),
    );
    container.set_property(
        "get",
        Value::function(|_, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            if options.is_object()
                && (!options.get_property("publicKey").is_undefined()
                    || !options.get_property("digital").is_undefined())
            {
                warn_secure_store();
                return w3cos_core::promise::reject(vec![error(
                    "NotSupportedError",
                    "public-key and digital credentials require a platform authenticator",
                )]);
            }
            warn_secure_store();
            w3cos_core::promise::resolve(vec![Value::Null])
        }),
    );
    container.set_property(
        "create",
        Value::function(|_, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            if !options.is_object() {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    "credentials.create requires a credential creation options object",
                )]);
            }
            let password = options.get_property("password");
            if password.is_object() {
                let credential =
                    w3cos_core::class::construct(&password_credential_class(), vec![password]);
                return w3cos_core::promise::resolve(vec![credential]);
            }
            let federated = options.get_property("federated");
            if federated.is_object() {
                let credential =
                    w3cos_core::class::construct(&federated_credential_class(), vec![federated]);
                return w3cos_core::promise::resolve(vec![credential]);
            }
            warn_secure_store();
            w3cos_core::promise::reject(vec![error(
                "NotSupportedError",
                "the requested credential type requires a platform authenticator",
            )])
        }),
    );
    container.set_property(
        "store",
        Value::function(|_, args| {
            let credential = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::class::instance_of(&credential, &credential_class()) {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    "credentials.store requires a Credential",
                )]);
            }
            warn_secure_store();
            w3cos_core::promise::reject(vec![error(
                "NotSupportedError",
                "secure credential persistence requires a platform credential vault",
            )])
        }),
    );
    container.set_property(
        "preventSilentAccess",
        Value::function(|_, _| {
            warn_secure_store();
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    container
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn creates_typed_credentials_without_claiming_secure_persistence() {
        let password = w3cos_core::class::construct(
            &password_credential_class(),
            vec![Value::object(HashMap::from([
                ("id".into(), Value::string("user")),
                ("password".into(), Value::string("secret")),
                ("name".into(), Value::string("User")),
            ]))],
        );
        assert!(w3cos_core::class::instance_of(
            &password,
            &credential_class()
        ));
        assert_eq!(password.get_property("type").to_js_string(), "password");

        let results = Rc::new(RefCell::new(Vec::<String>::new()));
        let container = credentials_container_value();
        for (promise, rejected, property) in [
            (
                container.call_method(
                    "create",
                    vec![Value::object(HashMap::from([(
                        "password".into(),
                        Value::object(HashMap::from([
                            ("id".into(), Value::string("created")),
                            ("password".into(), Value::string("value")),
                        ])),
                    )]))],
                ),
                false,
                "type",
            ),
            (
                container.call_method(
                    "get",
                    vec![Value::object(HashMap::from([(
                        "password".into(),
                        Value::Bool(true),
                    )]))],
                ),
                false,
                "",
            ),
            (
                container.call_method("store", vec![password.clone()]),
                true,
                "name",
            ),
        ] {
            let results_for_handler = Rc::clone(&results);
            let property = property.to_string();
            let handler = Value::function(move |_, args| {
                let value = if args[0].is_null() {
                    "null".into()
                } else {
                    args[0].get_property(&property).to_js_string()
                };
                results_for_handler.borrow_mut().push(value);
                Value::Undefined
            });
            promise.call_method(if rejected { "catch" } else { "then" }, vec![handler]);
        }
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            &*results.borrow(),
            &["password", "null", "NotSupportedError"]
        );
    }

    #[test]
    fn webauthn_capability_queries_are_truthful_without_an_authenticator() {
        let class = public_key_credential_class();
        let available = Rc::new(RefCell::new(Value::Undefined));
        let available_for_callback = Rc::clone(&available);
        class
            .call_method("isUserVerifyingPlatformAuthenticatorAvailable", Vec::new())
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *available_for_callback.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert!(!available.borrow().to_bool());
        let input = Value::object(HashMap::from([(
            "challenge".into(),
            Value::string("fixture"),
        )]));
        assert!(
            class
                .call_method("parseRequestOptionsFromJSON", vec![input.clone()])
                .strict_eq(&input)
        );
    }

    #[test]
    fn digital_identity_types_report_host_capabilities_truthfully() {
        let allowed = Rc::new(RefCell::new(Value::Undefined));
        let allowed_for_callback = Rc::clone(&allowed);
        digital_credential_class()
            .call_method("userAgentAllowsProtocol", vec![Value::string("openid4vp")])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *allowed_for_callback.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert!(!allowed.borrow().to_bool());
        assert_eq!(
            w3cos_core::class::get_prototype_of(
                &digital_credential_class().get_property("prototype")
            ),
            credential_class().get_property("prototype")
        );

        let nested = error("NetworkError", "provider unavailable");
        let identity_error = w3cos_core::class::construct(
            &identity_credential_error_class(),
            vec![
                Value::string("federated login failed"),
                Value::object(HashMap::from([
                    ("code".into(), Value::string("provider-error")),
                    ("error".into(), nested.clone()),
                    ("url".into(), Value::string("https://idp.example/error")),
                ])),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &identity_error,
            &crate::unsupported::dom_exception_class()
        ));
        assert_eq!(
            identity_error.get_property("code").to_js_string(),
            "provider-error"
        );
        assert!(identity_error.get_property("error") == nested);

        let rejected = Rc::new(RefCell::new(String::new()));
        let rejected_for_callback = Rc::clone(&rejected);
        identity_provider_class()
            .call_method("getUserInfo", vec![])
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *rejected_for_callback.borrow_mut() =
                        args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*rejected.borrow(), "NotSupportedError");
    }
}
