//! Local shell subject.
//!
//! This is the interface a later passkey-backed `did:key` would satisfy.
//! It does not generate a key, speak to a chain, hold a wallet, or verify a
//! WebAuthn assertion. The subject string is intentionally not a real DID.
//!
//! The unlock string is not stored. The stub keeps a PBKDF2-HMAC-SHA256
//! verifier (salt + hash). The subject is a `did:key:` string that can be
//! replaced later. Receipts already written keep the actor they recorded.
//!
//! Passkey enrollment can mint a challenge and store public-key bytes.
//! It does not speak to an authenticator and does not check an assertion.
//! TODO(webauthn): verify `clientDataJSON` against the stored challenge and
//! the assertion signature against the stored key before setting
//! [`IdentityMethod::Passkey`] or installing a `did:key` from that key.

use serde::{Deserialize, Serialize};

/// How a local subject was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMethod {
    /// Placeholder until a passkey-backed `did:key` exists.
    Stub,
    DidKey,
    Passkey,
}

/// How far passkey enrollment has gone. `AwaitingPasskey` is a note, not a
/// WebAuthn credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentState {
    #[default]
    Stub,
    /// A challenge exists. No credential and no checked assertion.
    AwaitingPasskey,
    /// Public-key bytes were stored for a challenge. The signature was not checked.
    KeyMaterialStored,
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
    method: IdentityMethod,
    enrollment: EnrollmentState,
    unlocked: bool,
    /// `pbkdf2-sha256$iterations$salt$hash`. Not the unlock string.
    verifier: Option<String>,
    /// base64url of 32 random bytes. Not a WebAuthn clientDataJSON.
    passkey_challenge: Option<String>,
    /// base64url of caller-supplied public-key bytes. Not attested.
    passkey_public_key: Option<String>,
}

impl LocalIdentityStub {
    pub const SUBJECT: &'static str = "did:key:stub.local";

    pub fn new() -> Self {
        Self {
            subject: Self::SUBJECT.to_string(),
            method: IdentityMethod::Stub,
            enrollment: EnrollmentState::Stub,
            unlocked: false,
            verifier: None,
            passkey_challenge: None,
            passkey_public_key: None,
        }
    }

    pub fn enrollment(&self) -> EnrollmentState {
        self.enrollment
    }

    /// Copy for the Settings identity section. This does not start WebAuthn.
    pub fn enrollment_copy(&self) -> String {
        match self.enrollment {
            EnrollmentState::Stub => {
                "WebAuthn 仪式还没做。当前是 did:key 占位，换成真钥匙时不改已有收据。Passkey ceremony is not implemented."
                    .to_string()
            }
            EnrollmentState::AwaitingPasskey => match self.passkey_challenge.as_deref() {
                Some(challenge) => format!(
                    "挑战 {challenge}。还没有验证 WebAuthn 断言。Challenge stored. No assertion was checked."
                ),
                None => "已记下注册意向。仍然没有 WebAuthn 凭证。Enrollment noted. No credential was created."
                    .to_string(),
            },
            EnrollmentState::KeyMaterialStored => {
                "公钥已保存，签名还没验证。Public key stored. The assertion was not checked."
                    .to_string()
            }
        }
    }

    /// Remember that the user asked to enroll. Does not create a challenge or a credential.
    pub fn note_passkey_enrollment(&mut self) {
        self.enrollment = EnrollmentState::AwaitingPasskey;
    }

    /// Mint a fresh 32-byte challenge. Clears any stored public key.
    /// Does not unlock, does not change the subject, and does not set
    /// [`IdentityMethod::Passkey`].
    ///
    /// TODO(webauthn): hand this challenge to a platform authenticator, then
    /// require `clientDataJSON.challenge` to match before trusting a key.
    pub fn begin_passkey_enrollment(&mut self) -> Option<String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).ok()?;
        let challenge = base64url_encode(&bytes);
        self.passkey_challenge = Some(challenge.clone());
        self.passkey_public_key = None;
        self.enrollment = EnrollmentState::AwaitingPasskey;
        Some(challenge)
    }

    pub fn passkey_challenge(&self) -> Option<&str> {
        self.passkey_challenge.as_deref()
    }

    pub fn passkey_public_key(&self) -> Option<&str> {
        self.passkey_public_key.as_deref()
    }

    /// Store public-key bytes against the current challenge.
    ///
    /// TODO(webauthn): this does not verify an attestation or an assertion,
    /// and it must not install a `did:key` from `public_key` until it does.
    pub fn store_passkey_public_key(&mut self, public_key: &[u8]) -> bool {
        if self.passkey_challenge.is_none() || public_key.is_empty() || public_key.len() > 128 {
            return false;
        }
        self.passkey_public_key = Some(base64url_encode(public_key));
        self.enrollment = EnrollmentState::KeyMaterialStored;
        true
    }

    pub fn restore_passkey_material(
        &mut self,
        challenge: Option<String>,
        public_key: Option<String>,
    ) {
        self.passkey_challenge = challenge.filter(|value| is_stored_challenge(value));
        self.passkey_public_key = public_key.filter(|value| is_stored_public_key(value));
    }

    pub fn restore_enrollment(&mut self, state: EnrollmentState) {
        self.enrollment = state;
    }

    /// Install a persisted `did:key:` subject. Returns false when `subject`
    /// is not a did:key. Does not touch receipts.
    pub fn restore_subject(&mut self, subject: &str) -> bool {
        self.install_subject(subject)
    }

    /// Replace the stub subject with another `did:key:`. Existing receipt
    /// records are left alone; only later actions use the new subject.
    pub fn install_subject(&mut self, subject: &str) -> bool {
        if !is_did_key(subject) {
            return false;
        }
        self.method = if subject.contains("stub") {
            IdentityMethod::Stub
        } else {
            IdentityMethod::DidKey
        };
        self.subject = subject.to_string();
        true
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

fn is_base64url(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn is_stored_challenge(value: &str) -> bool {
    value.len() == 43 && is_base64url(value)
}

fn is_stored_public_key(value: &str) -> bool {
    (2..=172).contains(&value.len()) && is_base64url(value)
}

fn base64url_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= data.len() {
        let chunk = u32::from_be_bytes([0, data[index], data[index + 1], data[index + 2]]);
        for shift in [18, 12, 6, 0] {
            out.push(TABLE[((chunk >> shift) & 0x3f) as usize] as char);
        }
        index += 3;
    }
    let rest = data.len() - index;
    if rest == 1 {
        let chunk = u32::from(data[index]) << 16;
        out.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
    } else if rest == 2 {
        let chunk = (u32::from(data[index]) << 16) | (u32::from(data[index + 1]) << 8);
        out.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn is_did_key(subject: &str) -> bool {
    let Some(rest) = subject.strip_prefix("did:key:") else {
        return false;
    };
    let len = rest.len();
    (8..=200).contains(&len) && rest.chars().all(|ch| !ch.is_whitespace())
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
        self.method
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

    #[test]
    fn a_did_key_can_replace_the_stub_without_a_ceremony() {
        let mut id = LocalIdentityStub::new();
        assert!(!id.install_subject("not-a-did"));
        assert!(!id.install_subject("did:key:short"));
        assert!(!id.install_subject("did:key:has space"));
        assert_eq!(id.subject(), LocalIdentityStub::SUBJECT);
        assert_eq!(id.method(), IdentityMethod::Stub);
        assert!(id.install_subject("did:key:z6Mkexample"));
        assert_eq!(id.subject(), "did:key:z6Mkexample");
        assert_eq!(id.method(), IdentityMethod::DidKey);
        assert!(!id.unlocked());
        assert!(id.remembered_pin().is_none());
        id.note_passkey_enrollment();
        assert_eq!(id.enrollment(), EnrollmentState::AwaitingPasskey);
        assert!(!id.unlocked());
        assert!(id.remembered_pin().is_none());
        assert!(id.enrollment_copy().contains("Enrollment noted"));
        assert!(id.enrollment_copy().contains("没有 WebAuthn"));
    }

    #[test]
    fn passkey_challenge_is_stored_without_a_ceremony() {
        let mut id = LocalIdentityStub::new();
        assert!(!id.store_passkey_public_key(&[1, 2, 3, 4]));
        let first = id.begin_passkey_enrollment().expect("challenge");
        let second = id.begin_passkey_enrollment().expect("another challenge");
        assert_eq!(first.len(), 43, "{first}");
        assert_ne!(first, second);
        assert!(!first.contains('='));
        assert_eq!(id.passkey_challenge(), Some(second.as_str()));
        assert!(id.passkey_public_key().is_none());
        assert_eq!(id.enrollment(), EnrollmentState::AwaitingPasskey);
        assert_eq!(id.method(), IdentityMethod::Stub);
        assert_eq!(id.subject(), LocalIdentityStub::SUBJECT);
        assert!(!id.unlocked());
        assert!(id.enrollment_copy().contains(&second));
        assert!(id.enrollment_copy().contains("No assertion was checked"));
        assert!(!id.store_passkey_public_key(&[]));
        assert!(!id.store_passkey_public_key(&[0u8; 129]));
        let key = [7u8; 32];
        assert!(id.store_passkey_public_key(&key));
        assert_eq!(id.enrollment(), EnrollmentState::KeyMaterialStored);
        let stored = id.passkey_public_key().expect("public key");
        assert!(stored.len() >= 2 && !stored.contains('='));
        assert_eq!(id.method(), IdentityMethod::Stub);
        assert_eq!(id.subject(), LocalIdentityStub::SUBJECT);
        assert!(!id.unlocked());
        assert!(id.remembered_pin().is_none());
        assert!(id.enrollment_copy().contains("was not checked"));
    }
}
