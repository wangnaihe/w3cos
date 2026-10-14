//! Local shell subject.
//!
//! This is the interface a later passkey-backed `did:key` would satisfy.
//! It does not generate a key, speak to a chain, hold a wallet, or verify a
//! WebAuthn assertion. The subject string is intentionally not a real DID.

/// How a local subject was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityMethod {
    /// Placeholder until a passkey-backed `did:key` exists.
    Stub,
    DidKey,
    Passkey,
}

/// What the shell needs from a local identity service.
pub trait LocalIdentity {
    fn subject(&self) -> &str;
    fn method(&self) -> IdentityMethod;
    fn unlocked(&self) -> bool;
}

/// In-memory stand-in. It boots locked. The first non-empty unlock string is
/// remembered for this process; later unlocks must repeat it. Nothing is
/// checked against an authenticator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIdentityStub {
    subject: String,
    unlocked: bool,
    /// First non-empty assertion. Later unlocks must repeat it. This is not a
    /// passkey; it only stops a different string from unlocking the same stub.
    pin: Option<String>,
}

impl LocalIdentityStub {
    pub const SUBJECT: &'static str = "did:key:stub.local";

    pub fn new() -> Self {
        Self {
            subject: Self::SUBJECT.to_string(),
            unlocked: false,
            pin: None,
        }
    }

    pub fn lock(&mut self) {
        self.unlocked = false;
    }

    pub fn remembered_pin(&self) -> Option<&str> {
        self.pin.as_deref()
    }

    /// Install a pin while staying locked. A ledger reload uses this so the
    /// first unlock string still matches after a restart. Does not replace a
    /// pin that is already set.
    pub fn restore_locked_pin(&mut self, pin: &str) {
        if pin.is_empty() || self.pin.is_some() {
            return;
        }
        self.pin = Some(pin.to_string());
        self.unlocked = false;
    }

    /// Returns false when `assertion` is empty. The first non-empty value is
    /// remembered; a later unlock with a different string fails. Nothing here
    /// is checked against an authenticator.
    pub fn unlock_with_passkey_stub(&mut self, assertion: &str) -> bool {
        if assertion.is_empty() {
            return false;
        }
        if let Some(pin) = &self.pin {
            if pin != assertion {
                return false;
            }
        } else {
            self.pin = Some(assertion.to_string());
        }
        self.unlocked = true;
        true
    }
}

impl Default for LocalIdentityStub {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalIdentity for LocalIdentityStub {
    fn subject(&self) -> &str {
        &self.subject
    }

    fn method(&self) -> IdentityMethod {
        IdentityMethod::Stub
    }

    fn unlocked(&self) -> bool {
        self.unlocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_subject_is_obviously_not_a_real_did_key() {
        let id = LocalIdentityStub::new();
        assert_eq!(id.subject(), LocalIdentityStub::SUBJECT);
        assert!(id.subject().starts_with("did:key:stub"));
        assert_eq!(id.method(), IdentityMethod::Stub);
        assert!(!id.unlocked());
        assert_ne!(id.method(), IdentityMethod::DidKey);
        assert_ne!(id.method(), IdentityMethod::Passkey);
    }

    #[test]
    fn empty_passkey_stub_does_not_unlock() {
        let mut id = LocalIdentityStub::new();
        id.lock();
        assert!(!id.unlocked());
        assert!(!id.unlock_with_passkey_stub(""));
        assert!(!id.unlocked());
        assert!(id.unlock_with_passkey_stub("not-a-real-passkey"));
        assert!(id.unlocked());
        id.lock();
        assert!(!id.unlock_with_passkey_stub("other-string"));
        assert!(!id.unlocked());
        assert!(id.unlock_with_passkey_stub("not-a-real-passkey"));
        assert!(id.unlocked());
    }
}
