//! Origin-scoped secret store behind `navigator.w3cos.keyring`.
//!
//! Each document origin gets its own service name. `get`, `delete`, and
//! `list` keep an item only when the store reports that exact service, so
//! one app cannot read another origin's entries even when they share the
//! operating-system keyring.
//!
//! Desktop calls go through the `keyring` crate: Secret Service on Linux
//! (the D-Bus API libsecret uses), Keychain Services on macOS, and
//! Credential Manager on Windows. There is no file fallback. A missing or
//! unreachable store rejects the call. Tests install an in-memory store and
//! never open the host keyring.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use keyring_core::Error as StoreError;

const SERVICE_LABEL: &str = "org.w3cos.keyring";
const MAX_NAME_BYTES: usize = 512;
const MAX_SECRET_BYTES: usize = 48 * 1024;

enum Backend {
    Platform,
    Store(Arc<keyring_core::CredentialStore>),
    Unavailable(String),
}

thread_local! {
    static BACKEND: std::cell::RefCell<Backend> = const { std::cell::RefCell::new(Backend::Platform) };
}

#[derive(Debug)]
pub enum KeyringError {
    Type(&'static str),
    Unavailable(String),
}

impl KeyringError {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Type(_) => "TypeError",
            Self::Unavailable(_) => "NotSupportedError",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Type(message) => message,
            Self::Unavailable(message) => message.as_str(),
        }
    }
}

pub fn reset_override() {
    BACKEND.with(|backend| *backend.borrow_mut() = Backend::Platform);
}

pub fn install_mock() -> Arc<keyring_core::CredentialStore> {
    let store: Arc<keyring_core::CredentialStore> =
        keyring_core::mock::Store::new().expect("in-memory keyring");
    BACKEND.with(|backend| *backend.borrow_mut() = Backend::Store(store.clone()));
    store
}

pub fn install_unavailable(reason: impl Into<String>) {
    BACKEND.with(|backend| *backend.borrow_mut() = Backend::Unavailable(reason.into()));
}

pub fn set_secret(name: &str, secret: &str) -> Result<(), KeyringError> {
    validate_name(name)?;
    validate_secret(secret)?;
    let service = service_for_origin(&current_origin());
    let account = encode_component(name);
    with_store(|store| {
        let entry = store
            .build(&service, &account, None)
            .map_err(store_failure)?;
        entry.set_password(secret).map_err(store_failure)?;
        Ok(())
    })
}

pub fn get_secret(name: &str) -> Result<Option<String>, KeyringError> {
    validate_name(name)?;
    let service = service_for_origin(&current_origin());
    let account = encode_component(name);
    with_store(|store| {
        let entry = store
            .build(&service, &account, None)
            .map_err(store_failure)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(StoreError::NoEntry) => Ok(None),
            Err(err) => Err(store_failure(err)),
        }
    })
}

pub fn delete_secret(name: &str) -> Result<(), KeyringError> {
    validate_name(name)?;
    let service = service_for_origin(&current_origin());
    let account = encode_component(name);
    with_store(|store| {
        let entry = store
            .build(&service, &account, None)
            .map_err(store_failure)?;
        match entry.delete_credential() {
            Ok(()) | Err(StoreError::NoEntry) => Ok(()),
            Err(err) => Err(store_failure(err)),
        }
    })
}

pub fn list_secrets() -> Result<Vec<String>, KeyringError> {
    let service = service_for_origin(&current_origin());
    with_store(|store| list_service(store, &service))
}

fn with_store<T>(
    op: impl FnOnce(&keyring_core::CredentialStore) -> Result<T, KeyringError>,
) -> Result<T, KeyringError> {
    let overridden = BACKEND.with(|backend| match &*backend.borrow() {
        Backend::Platform => None,
        Backend::Store(store) => Some(Ok(store.clone())),
        Backend::Unavailable(reason) => Some(Err(KeyringError::Unavailable(reason.clone()))),
    });
    if let Some(result) = overridden {
        let store = result?;
        return op(store.as_ref());
    }
    let store = platform_store()?;
    op(store.as_ref())
}

fn list_service(
    store: &keyring_core::CredentialStore,
    service: &str,
) -> Result<Vec<String>, KeyringError> {
    let pattern = windows_target_pattern(service);
    let spec = list_spec(service, &pattern);
    let entries = store.search(&spec).map_err(store_failure)?;
    let mut names = Vec::new();
    for entry in entries {
        let specifiers = match entry.get_specifiers() {
            Some(specifiers) => specifiers,
            // A locked collection reports no specifiers. Surface that instead
            // of an empty list, and do not read entries that are not ours.
            None => {
                if let Err(err) = entry.get_credential()
                    && is_store_access_error(&err)
                {
                    return Err(store_failure(err));
                }
                continue;
            }
        };
        let (found_service, account) = specifiers;
        if found_service != service {
            continue;
        }
        let Some(name) = decode_component(&account) else {
            continue;
        };
        match entry.get_credential() {
            Ok(_) => names.push(name),
            Err(StoreError::NoEntry) => {}
            Err(err) => return Err(store_failure(err)),
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(target_os = "windows")]
fn list_spec<'a>(_service: &'a str, pattern: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("pattern", pattern)])
}

#[cfg(not(target_os = "windows"))]
fn list_spec<'a>(service: &'a str, _pattern: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("service", service)])
}

fn windows_target_pattern(service: &str) -> String {
    // Credential Manager's default target is `{account}.{service}`. Both
    // halves are percent-encoded, so the first dot is the delimiter.
    // The encoded service has no regex metacharacters.
    format!("^[^.]+\\.{service}$")
}

fn current_origin() -> String {
    let origin = crate::history::get_origin();
    if origin.is_empty() {
        "null".to_string()
    } else {
        origin
    }
}

fn service_for_origin(origin: &str) -> String {
    encode_component(&format!("{SERVICE_LABEL}:{origin}"))
}

fn validate_name(name: &str) -> Result<(), KeyringError> {
    if name.is_empty() {
        return Err(KeyringError::Type("keyring name must not be empty"));
    }
    if name.as_bytes().contains(&0) {
        return Err(KeyringError::Type("keyring name must not contain NUL"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(KeyringError::Type("keyring name is too long"));
    }
    Ok(())
}

fn validate_secret(secret: &str) -> Result<(), KeyringError> {
    if secret.as_bytes().contains(&0) {
        return Err(KeyringError::Type("keyring secret must not contain NUL"));
    }
    if secret.len() > MAX_SECRET_BYTES {
        return Err(KeyringError::Type("keyring secret is too long"));
    }
    Ok(())
}

fn is_store_access_error(err: &StoreError) -> bool {
    matches!(
        err,
        StoreError::PlatformFailure(_)
            | StoreError::NoStorageAccess(_)
            | StoreError::NoDefaultStore
            | StoreError::BadStoreFormat(_)
    )
}

fn store_failure(err: StoreError) -> KeyringError {
    // Every store failure stays `NotSupportedError`. The message names the
    // failure class and never includes the secret or the raw platform error,
    // which can echo credential bytes.
    KeyringError::Unavailable(format!(
        "navigator.w3cos.keyring cannot use the system secret store ({}); the call was refused",
        public_message(&err)
    ))
}

fn public_message(err: &StoreError) -> String {
    match err {
        StoreError::PlatformFailure(_) => "the platform secret store failed".to_string(),
        StoreError::NoStorageAccess(_) => "the secret store is locked or unreachable".to_string(),
        StoreError::NoEntry => "no matching credential".to_string(),
        StoreError::BadEncoding(_) => "the stored secret is not text".to_string(),
        StoreError::BadDataFormat(_, _) => "the stored secret could not be read".to_string(),
        StoreError::BadStoreFormat(_) => "the secret store is unreadable".to_string(),
        StoreError::TooLong(name, limit) => {
            format!("{name} exceeds the platform limit of {limit} characters")
        }
        StoreError::Invalid(field, _) if field == "platform" => {
            "this operating system has no Secret Service, Keychain, or Credential Manager adapter"
                .to_string()
        }
        StoreError::Invalid(field, _) => {
            format!("the secret store rejected {}", safe_field(field))
        }
        StoreError::Ambiguous(_) => {
            "more than one credential matched; refusing to choose".to_string()
        }
        StoreError::NoDefaultStore => "no secret store is open".to_string(),
        StoreError::NotSupportedByStore(_) => {
            "this secret store does not support the operation".to_string()
        }
        _ => "the secret store rejected the call".to_string(),
    }
}

fn safe_field(field: &str) -> &str {
    match field {
        "service" | "user" | "username" | "password" | "secret" | "platform" | "target"
        | "name" => field,
        _ => "a field",
    }
}

fn platform_store() -> Result<Arc<keyring_core::CredentialStore>, KeyringError> {
    open_desktop_store()
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn open_desktop_store() -> Result<Arc<keyring_core::CredentialStore>, KeyringError> {
    static READY: OnceLock<Result<(), String>> = OnceLock::new();
    let ready = READY.get_or_init(|| {
        if session_bus_missing() {
            return Err(
                "navigator.w3cos.keyring cannot reach the system secret store (no D-Bus session bus; Secret Service is unavailable); the call was refused"
                    .to_string(),
            );
        }
        match keyring::Entry::store_status() {
            Ok(()) => Ok(()),
            Err(err) => Err(format!(
                "navigator.w3cos.keyring cannot reach the system secret store ({}); the call was refused",
                public_message(err)
            )),
        }
    });
    if let Err(message) = ready {
        return Err(KeyringError::Unavailable(message.clone()));
    }
    keyring_core::get_default_store().ok_or_else(|| {
        KeyringError::Unavailable(
            "navigator.w3cos.keyring cannot reach the system secret store (no secret store is open); the call was refused"
                .to_string(),
        )
    })
}

#[cfg(not(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn open_desktop_store() -> Result<Arc<keyring_core::CredentialStore>, KeyringError> {
    Err(KeyringError::Unavailable(
        "navigator.w3cos.keyring has no platform adapter on this operating system; the call was refused"
            .to_string(),
    ))
}

/// Secret Service needs a session bus. Headless CI usually has neither the
/// address nor `$XDG_RUNTIME_DIR/bus`. Skipping the connect avoids a hang
/// and reports the same refusal a failed open would.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn session_bus_missing() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return false;
    }
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return true;
    };
    !std::path::Path::new(&runtime).join("bus").exists()
}

#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
fn session_bus_missing() -> bool {
    false
}

fn encode_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => out.push(*byte as char),
            byte => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

fn decode_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_value(bytes[index + 1])?;
            let lo = hex_value(bytes[index + 2])?;
            out.push((hi << 4) | lo);
            index += 3;
        } else {
            match bytes[index] {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => out.push(bytes[index]),
                _ => return None,
            }
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_round_trips_and_hides_the_windows_delimiter() {
        let origin = "https://app.example:8443";
        let service = service_for_origin(origin);
        assert!(!service.contains('.'));
        assert!(service.starts_with("org%2Ew3cos%2Ekeyring%3A"));
        let account = encode_component("api token.v2");
        assert!(!account.contains('.'));
        assert_eq!(decode_component(&account).as_deref(), Some("api token.v2"));
        let other = service_for_origin("https://app.example:8443.evil");
        assert!(other.contains(&service));
        assert_ne!(other, service);
    }

    #[test]
    fn public_errors_do_not_include_secret_bytes() {
        let secret = b"super-secret-value".to_vec();
        let err = StoreError::BadEncoding(secret.clone());
        let message = public_message(&err);
        assert!(!message.contains("super-secret-value"));
        let ambiguous = StoreError::Ambiguous(Vec::new());
        assert!(!public_message(&ambiguous).contains("super-secret"));
        let invalid = StoreError::Invalid("password".into(), String::from_utf8(secret).unwrap());
        assert!(!public_message(&invalid).contains("super-secret-value"));
    }

    #[test]
    fn mock_store_keeps_origins_apart() {
        reset_override();
        crate::history::reset();
        install_mock();
        crate::history::location_replace("https://one.example/");
        set_secret("token", "one-secret").unwrap();
        set_secret("api token.v2", "dotted").unwrap();
        assert_eq!(get_secret("token").unwrap().as_deref(), Some("one-secret"));
        let mut listed = list_secrets().unwrap();
        listed.sort();
        assert_eq!(
            listed,
            vec!["api token.v2".to_string(), "token".to_string()]
        );

        crate::history::location_replace("https://one.example.evil/");
        assert_eq!(get_secret("token").unwrap(), None);
        assert!(list_secrets().unwrap().is_empty());
        set_secret("token", "evil-secret").unwrap();

        crate::history::location_replace("https://one.example/");
        assert_eq!(get_secret("token").unwrap().as_deref(), Some("one-secret"));
        delete_secret("token").unwrap();
        delete_secret("missing").unwrap();
        assert_eq!(get_secret("token").unwrap(), None);
        assert_eq!(list_secrets().unwrap(), vec!["api token.v2".to_string()]);
        crate::history::reset();
        reset_override();
    }

    #[test]
    fn unavailable_backend_refuses_without_echoing_the_secret() {
        reset_override();
        install_unavailable(
            "navigator.w3cos.keyring cannot reach the system secret store (no D-Bus session bus; Secret Service is unavailable); the call was refused",
        );
        let err = set_secret("token", "super-secret-value").unwrap_err();
        assert_eq!(err.name(), "NotSupportedError");
        assert!(err.message().contains("Secret Service"));
        assert!(!err.message().contains("super-secret-value"));
        assert!(get_secret("token").is_err());
        reset_override();
    }

    #[test]
    fn headless_linux_refuses_before_writing_a_secret() {
        if !session_bus_missing() {
            return;
        }
        reset_override();
        let err = set_secret("token", "super-secret-value").unwrap_err();
        assert_eq!(err.name(), "NotSupportedError");
        assert!(err.message().contains("Secret Service"));
        assert!(!err.message().contains("super-secret-value"));
        assert!(get_secret("token").is_err());
        reset_override();
    }

    #[test]
    fn names_and_secrets_reject_empty_and_nul() {
        reset_override();
        install_mock();
        assert_eq!(set_secret("", "x").unwrap_err().name(), "TypeError");
        assert_eq!(set_secret("name\0", "x").unwrap_err().name(), "TypeError");
        assert_eq!(set_secret("name", "x\0").unwrap_err().name(), "TypeError");
        assert_eq!(get_secret("").unwrap_err().name(), "TypeError");
        reset_override();
    }
}
