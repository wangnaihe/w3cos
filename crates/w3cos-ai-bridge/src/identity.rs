//! Local shell subject.
//!
//! This is the interface a later passkey-backed `did:key` would satisfy.
//! It does not generate a key, speak to a chain, hold a wallet, or verify a
//! WebAuthn assertion. The subject string is intentionally not a real DID.
//!
//! The unlock string is not stored. The stub keeps a PBKDF2-HMAC-SHA256
//! verifier (salt + hash). Passkey / `did:key` is still the next step; this
//! is only so a copied stub file is not the password itself.

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
/// stretched with PBKDF2 and only the verifier is kept. Later unlocks must
/// repeat that string. Nothing is checked against an authenticator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIdentityStub {
    subject: String,
    unlocked: bool,
    /// `pbkdf2-sha256$iterations$salt$hash`. Not the unlock string.
    verifier: Option<String>,
}

impl LocalIdentityStub {
    pub const SUBJECT: &'static str = "did:key:stub.local";

    pub fn new() -> Self {
        Self {
            subject: Self::SUBJECT.to_string(),
            unlocked: false,
            verifier: None,
        }
    }

    pub fn lock(&mut self) {
        self.unlocked = false;
    }

    /// Encoded verifier for the encrypted ledger. This is not the unlock string.
    pub fn remembered_pin(&self) -> Option<&str> {
        self.verifier.as_deref()
    }

    /// Install a stored verifier while staying locked. A legacy plaintext pin
    /// is stretched immediately. Returns true when the stored form changed and
    /// should be written back. Does not replace a verifier that is already set.
    pub fn restore_locked_pin(&mut self, stored: &str) -> bool {
        if stored.is_empty() || self.verifier.is_some() {
            return false;
        }
        let (verifier, migrated) = if is_verifier(stored) {
            (stored.to_string(), false)
        } else {
            let Some(verifier) = seal_pin(stored) else {
                return false;
            };
            (verifier, true)
        };
        self.verifier = Some(verifier);
        self.unlocked = false;
        migrated
    }

    /// Returns false when `assertion` is empty or does not match the verifier.
    /// The first non-empty value is stretched and stored. Nothing here is
    /// checked against an authenticator. Passkey / `did:key` is still next.
    pub fn unlock_with_passkey_stub(&mut self, assertion: &str) -> bool {
        if assertion.is_empty() {
            return false;
        }
        if let Some(stored) = &self.verifier {
            if !verify_pin(assertion, stored) {
                return false;
            }
        } else {
            let Some(verifier) = seal_pin(assertion) else {
                return false;
            };
            self.verifier = Some(verifier);
        }
        self.unlocked = true;
        true
    }
}

const PBKDF2_ITERATIONS: u32 = 100_000;
const VERIFIER_PREFIX: &str = "pbkdf2-sha256$";

fn is_verifier(stored: &str) -> bool {
    stored.starts_with(VERIFIER_PREFIX)
}

fn seal_pin(assertion: &str) -> Option<String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).ok()?;
    let hash = derive_pin(assertion, &salt)?;
    Some(format!(
        "{VERIFIER_PREFIX}{PBKDF2_ITERATIONS}${}${}",
        hex_encode(&salt),
        hex_encode(&hash)
    ))
}

fn verify_pin(assertion: &str, stored: &str) -> bool {
    let Some((salt, expected)) = parse_verifier(stored) else {
        return false;
    };
    let Ok(iterations) = std::num::NonZeroU32::new(PBKDF2_ITERATIONS).ok_or(()) else {
        return false;
    };
    ring::pbkdf2::verify(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        &salt,
        assertion.as_bytes(),
        &expected,
    )
    .is_ok()
}

fn derive_pin(assertion: &str, salt: &[u8]) -> Option<[u8; 32]> {
    let iterations = std::num::NonZeroU32::new(PBKDF2_ITERATIONS)?;
    let mut out = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        assertion.as_bytes(),
        &mut out,
    );
    Some(out)
}

fn parse_verifier(stored: &str) -> Option<([u8; 16], [u8; 32])> {
    let rest = stored.strip_prefix(VERIFIER_PREFIX)?;
    let mut parts = rest.split('$');
    let iterations: u32 = parts.next()?.parse().ok()?;
    if iterations != PBKDF2_ITERATIONS {
        return None;
    }
    let salt = hex_decode_array::<16>(parts.next()?)?;
    let hash = hex_decode_array::<32>(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((salt, hash))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let bytes = text.as_bytes();
    for (index, slot) in out.iter_mut().enumerate() {
        let hi = hex_nibble(bytes[index * 2])?;
        let lo = hex_nibble(bytes[index * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
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

    #[test]
    fn unlock_string_is_stored_only_as_a_pbkdf2_verifier() {
        let mut id = LocalIdentityStub::new();
        assert!(id.unlock_with_passkey_stub("alpha"));
        let stored = id.remembered_pin().expect("verifier").to_string();
        assert!(stored.starts_with("pbkdf2-sha256$100000$"), "{stored}");
        assert!(!stored.contains("alpha"), "{stored}");
        id.lock();
        assert!(!id.unlock_with_passkey_stub("beta"));
        assert!(!id.unlocked());
        assert!(id.unlock_with_passkey_stub("alpha"));

        let mut restored = LocalIdentityStub::new();
        assert!(!restored.restore_locked_pin(&stored));
        assert!(!restored.unlocked());
        assert!(!restored.unlock_with_passkey_stub("beta"));
        assert!(restored.unlock_with_passkey_stub("alpha"));

        let mut legacy = LocalIdentityStub::new();
        assert!(legacy.restore_locked_pin("alpha"));
        let migrated = legacy.remembered_pin().expect("migrated").to_string();
        assert!(migrated.starts_with("pbkdf2-sha256$"), "{migrated}");
        assert!(!migrated.contains("alpha"), "{migrated}");
        assert_ne!(migrated, "alpha");
        assert!(legacy.unlock_with_passkey_stub("alpha"));
        assert!(!legacy.restore_locked_pin("beta"));
        assert_eq!(legacy.remembered_pin(), Some(migrated.as_str()));
    }
}
