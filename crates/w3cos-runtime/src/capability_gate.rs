//! Explicit permission decisions for powerful Web APIs and `navigator.w3cos`.
//!
//! `prompt` and `denied` both refuse the operation. A capability runs only
//! after the host records `granted`. This store does not invent a grant.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static DECISIONS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

pub fn decision(name: &str) -> Option<String> {
    DECISIONS.with(|decisions| decisions.borrow().get(name).cloned())
}

pub fn is_granted(name: &str) -> bool {
    decision(name).as_deref() == Some("granted")
}

pub fn set_decision(name: &str, state: &str) {
    let state = match state {
        "granted" | "denied" | "prompt" => state.to_string(),
        _ => "denied".to_string(),
    };
    DECISIONS.with(|decisions| {
        decisions.borrow_mut().insert(name.to_string(), state);
    });
}

pub fn grant(name: &str) {
    set_decision(name, "granted");
}

pub fn deny(name: &str) {
    set_decision(name, "denied");
}

pub fn reset() {
    DECISIONS.with(|decisions| decisions.borrow_mut().clear());
}

pub fn not_allowed(name: &str) -> Value {
    w3cos_core::web::dom_exception_instance(
        &format!("{name} permission is not granted"),
        "NotAllowedError",
    )
}

pub fn require(name: &str) -> Result<(), Value> {
    if is_granted(name) {
        Ok(())
    } else {
        Err(not_allowed(name))
    }
}

/// `Some(rejected promise)` when the named permission has not been granted.
pub fn reject_unless_granted(name: &str) -> Option<Value> {
    require(name)
        .err()
        .map(|error| w3cos_core::promise::reject(vec![error]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_and_denied_refuse_until_an_explicit_grant() {
        reset();
        assert!(require("direct-sockets").is_err());
        deny("direct-sockets");
        assert!(require("direct-sockets").is_err());
        assert_eq!(decision("direct-sockets").as_deref(), Some("denied"));
        grant("direct-sockets");
        assert!(require("direct-sockets").is_ok());
        reset();
        assert!(decision("direct-sockets").is_none());
    }
}
