//! Local shell subject.
//!
//! A passkey is this device's Ed25519 key. [`accept_webauthn_assertion`]
//! checks `clientDataJSON` and the assertion signature before
//! [`IdentityMethod::Passkey`], then installs the canonical `did:key` for that
//! key. Receipts already written keep the actor they recorded. This module
//! does not replace the P2P seed and does not read the pairing fingerprint.
//!
//! The unlock string is not stored. The lock screen still keeps a
//! PBKDF2-HMAC-SHA256 verifier. That verifier is not a passkey.

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

/// How far passkey enrollment has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentState {
    #[default]
    Stub,
    /// A challenge exists. No credential and no checked assertion.
    AwaitingPasskey,
    /// Public-key bytes were stored for a challenge. The signature was not checked.
    KeyMaterialStored,
    /// `clientDataJSON` and the assertion signature matched this device key.
    AssertionVerified,
}

/// Why an assertion was refused. The subject is left as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionFault {
    Malformed,
    Challenge,
    Origin,
    Authenticator,
    Key,
    Signature,
}

/// Origin inside `clientDataJSON`. The rpId is the host of this origin.
pub const WEBAUTHN_ORIGIN: &str = "https://shell.w3cos.local";
/// Relying party id. `authenticatorData` must carry SHA-256 of this string.
pub const WEBAUTHN_RP_ID: &str = "shell.w3cos.local";

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

    /// Short status for tests and the ledger. The shell paints Fluent strings.
    pub fn enrollment_copy(&self) -> String {
        match self.enrollment {
            EnrollmentState::Stub => {
                "Passkey is not registered. No assertion was checked.".to_string()
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
            EnrollmentState::AssertionVerified => {
                "Assertion checked. Passkey is bound to the did:key.".to_string()
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
        if state == EnrollmentState::AssertionVerified
            && is_did_key(&self.subject)
            && !self.subject.contains("stub")
        {
            self.method = IdentityMethod::Passkey;
        }
    }

    /// Check a WebAuthn assertion and bind the subject to the canonical
    /// `did:key` of `device_public`.
    ///
    /// The public key in the assertion must be that same 32-byte Ed25519 key.
    /// A pairing fingerprint is not a public key and does not match. A failed
    /// check leaves the subject, the method, and the unlock verifier alone.
    pub fn accept_webauthn_assertion(
        &mut self,
        assertion: &str,
        device_public: &[u8; 32],
    ) -> Result<(), AssertionFault> {
        if assertion.len() > 8192 {
            return Err(AssertionFault::Malformed);
        }
        let value: serde_json::Value =
            serde_json::from_str(assertion).map_err(|_| AssertionFault::Malformed)?;
        let client_b64 = value
            .get("clientDataJSON")
            .and_then(|item| item.as_str())
            .ok_or(AssertionFault::Malformed)?;
        let auth_b64 = value
            .get("authenticatorData")
            .and_then(|item| item.as_str())
            .ok_or(AssertionFault::Malformed)?;
        let sig_b64 = value
            .get("signature")
            .and_then(|item| item.as_str())
            .ok_or(AssertionFault::Malformed)?;
        let key_b64 = value
            .get("publicKey")
            .and_then(|item| item.as_str())
            .ok_or(AssertionFault::Key)?;
        let client = base64url_decode(client_b64).ok_or(AssertionFault::Malformed)?;
        let auth = base64url_decode(auth_b64).ok_or(AssertionFault::Malformed)?;
        let signature = base64url_decode(sig_b64).ok_or(AssertionFault::Malformed)?;
        let public = base64url_decode(key_b64).ok_or(AssertionFault::Key)?;
        if public.as_slice() != device_public.as_slice() {
            return Err(AssertionFault::Key);
        }
        let parsed: serde_json::Value =
            serde_json::from_slice(&client).map_err(|_| AssertionFault::Malformed)?;
        if parsed.get("type").and_then(|item| item.as_str()) != Some("webauthn.get") {
            return Err(AssertionFault::Malformed);
        }
        let challenge = parsed
            .get("challenge")
            .and_then(|item| item.as_str())
            .ok_or(AssertionFault::Challenge)?;
        let stored = self
            .passkey_challenge
            .as_deref()
            .ok_or(AssertionFault::Challenge)?;
        if !constant_eq(challenge, stored) {
            return Err(AssertionFault::Challenge);
        }
        let origin = parsed
            .get("origin")
            .and_then(|item| item.as_str())
            .ok_or(AssertionFault::Origin)?;
        if origin != WEBAUTHN_ORIGIN
            || parsed.get("crossOrigin").and_then(|item| item.as_bool()) == Some(true)
        {
            return Err(AssertionFault::Origin);
        }
        if auth.len() < 37 || signature.len() != 64 {
            return Err(AssertionFault::Authenticator);
        }
        let rp_hash = sha256(WEBAUTHN_RP_ID.as_bytes());
        if auth[..32] != rp_hash.as_ref()[..] {
            return Err(AssertionFault::Authenticator);
        }
        let flags = auth[32];
        if flags & 0x01 == 0 || flags & 0x04 == 0 {
            return Err(AssertionFault::Authenticator);
        }
        let mut message = Vec::with_capacity(auth.len() + 32);
        message.extend_from_slice(&auth);
        message.extend_from_slice(sha256(&client).as_ref());
        let key = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ED25519,
            device_public.as_slice(),
        );
        if key.verify(&message, &signature).is_err() {
            return Err(AssertionFault::Signature);
        }
        let did = canonical_did(device_public);
        if !self.install_subject(&did) {
            return Err(AssertionFault::Key);
        }
        self.method = IdentityMethod::Passkey;
        self.enrollment = EnrollmentState::AssertionVerified;
        self.passkey_public_key = Some(base64url_encode(device_public));
        Ok(())
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

    /// Returns false when `passphrase` is empty or does not match the verifier.
    /// The first non-empty value is stretched with PBKDF2-HMAC-SHA256 and only
    /// the verifier is stored. This is the lock screen, not a passkey.
    pub fn unlock_with_passphrase(&mut self, passphrase: &str) -> bool {
        if passphrase.is_empty() {
            return false;
        }
        if let Some(stored) = &self.verifier {
            if !verify_pin(passphrase, stored) {
                return false;
            }
        } else {
            let Some(verifier) = seal_pin(passphrase) else {
                return false;
            };
            self.verifier = Some(verifier);
        }
        self.unlocked = true;
        true
    }

    /// Older name for [`unlock_with_passphrase`](Self::unlock_with_passphrase).
    pub fn unlock_with_passkey_stub(&mut self, assertion: &str) -> bool {
        self.unlock_with_passphrase(assertion)
    }
}

const PBKDF2_ITERATIONS: u32 = 100_000;
const VERIFIER_PREFIX: &str = "pbkdf2-sha256$";

/// Build a WebAuthn assertion for `challenge`, signed by `sign`.
///
/// `sign` is the device Ed25519 signer. This function does not hold the seed.
pub fn device_assertion(
    challenge: &str,
    public: &[u8; 32],
    sign: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
) -> Option<String> {
    if !is_stored_challenge(challenge) {
        return None;
    }
    let client = format!(
        "{{\"type\":\"webauthn.get\",\"challenge\":\"{challenge}\",\"origin\":\"{WEBAUTHN_ORIGIN}\",\"crossOrigin\":false}}"
    );
    let auth = authenticator_data();
    let mut message = auth.to_vec();
    message.extend_from_slice(sha256(client.as_bytes()).as_ref());
    let signature = sign(&message)?;
    if signature.len() != 64 {
        return None;
    }
    Some(
        serde_json::json!({
            "type": "public-key",
            "clientDataJSON": base64url_encode(client.as_bytes()),
            "authenticatorData": base64url_encode(&auth),
            "signature": base64url_encode(&signature),
            "publicKey": base64url_encode(public),
        })
        .to_string(),
    )
}

fn authenticator_data() -> [u8; 37] {
    let mut out = [0u8; 37];
    let rp_hash = sha256(WEBAUTHN_RP_ID.as_bytes());
    out[..32].copy_from_slice(rp_hash.as_ref());
    out[32] = 0x01 | 0x04;
    out
}

fn sha256(data: &[u8]) -> ring::digest::Digest {
    ring::digest::digest(&ring::digest::SHA256, data)
}

fn canonical_did(public: &[u8; 32]) -> String {
    let mut payload = Vec::with_capacity(34);
    payload.extend_from_slice(&[0xed, 0x01]);
    payload.extend_from_slice(public);
    format!("did:key:z{}", bs58::encode(payload).into_string())
}

fn constant_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

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

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || text.len() > 4096 || !is_base64url(text) {
        return None;
    }
    fn value(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let data = text.as_bytes();
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut index = 0;
    while index + 4 <= data.len() {
        let chunk = (value(data[index])? << 18)
            | (value(data[index + 1])? << 12)
            | (value(data[index + 2])? << 6)
            | value(data[index + 3])?;
        out.push((chunk >> 16) as u8);
        out.push((chunk >> 8) as u8);
        out.push(chunk as u8);
        index += 4;
    }
    let rest = data.len() - index;
    if rest == 1 {
        return None;
    }
    if rest == 2 {
        let chunk = (value(data[index])? << 18) | (value(data[index + 1])? << 12);
        out.push((chunk >> 16) as u8);
    } else if rest == 3 {
        let chunk = (value(data[index])? << 18)
            | (value(data[index + 1])? << 12)
            | (value(data[index + 2])? << 6);
        out.push((chunk >> 16) as u8);
        out.push((chunk >> 8) as u8);
    }
    Some(out)
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
        assert!(!id.unlock_with_passphrase(""));
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

    #[test]
    fn a_checked_assertion_binds_the_canonical_did_and_rejects_the_rest() {
        let device = w3cos_p2p::DeviceIdentity::from_seed([9u8; 32]);
        let other = w3cos_p2p::DeviceIdentity::from_seed([4u8; 32]);
        assert_eq!(canonical_did(&device.ed25519_public), device.did_key());
        assert_ne!(device.did_key(), device.fingerprint());
        assert!(device.fingerprint().starts_with("did:key:z"));

        let mut id = LocalIdentityStub::new();
        assert!(id.unlock_with_passphrase("alpha"));
        let verifier = id.remembered_pin().expect("verifier").to_string();
        let challenge = id.begin_passkey_enrollment().expect("challenge");
        let assertion = device_assertion(&challenge, &device.ed25519_public, |message| {
            device.sign(message).ok()
        })
        .expect("assertion");

        assert_eq!(
            id.accept_webauthn_assertion(&assertion, &other.ed25519_public),
            Err(AssertionFault::Key)
        );
        assert_eq!(id.subject(), LocalIdentityStub::SUBJECT);
        assert_eq!(id.method(), IdentityMethod::Stub);

        let mut flipped: serde_json::Value = serde_json::from_str(&assertion).unwrap();
        let signature = flipped["signature"].as_str().unwrap().to_string();
        let mut bad = signature.into_bytes();
        bad[0] = if bad[0] == b'A' { b'B' } else { b'A' };
        flipped["signature"] = serde_json::Value::String(String::from_utf8(bad).unwrap());
        assert_eq!(
            id.accept_webauthn_assertion(&flipped.to_string(), &device.ed25519_public),
            Err(AssertionFault::Signature)
        );
        assert_eq!(id.subject(), LocalIdentityStub::SUBJECT);

        let mut client = format!(
            "{{\"type\":\"webauthn.get\",\"challenge\":\"{challenge}\",\"origin\":\"https://evil.example\",\"crossOrigin\":false}}"
        );
        let mut forged = serde_json::from_str::<serde_json::Value>(&assertion).unwrap();
        forged["clientDataJSON"] = serde_json::Value::String(base64url_encode(client.as_bytes()));
        assert_eq!(
            id.accept_webauthn_assertion(&forged.to_string(), &device.ed25519_public),
            Err(AssertionFault::Origin)
        );

        client = format!(
            "{{\"type\":\"webauthn.create\",\"challenge\":\"{challenge}\",\"origin\":\"{WEBAUTHN_ORIGIN}\",\"crossOrigin\":false}}"
        );
        forged["clientDataJSON"] = serde_json::Value::String(base64url_encode(client.as_bytes()));
        assert_eq!(
            id.accept_webauthn_assertion(&forged.to_string(), &device.ed25519_public),
            Err(AssertionFault::Malformed)
        );

        forged = serde_json::from_str(&assertion).unwrap();
        forged["publicKey"] =
            serde_json::Value::String(base64url_encode(device.fingerprint().as_bytes()));
        assert_eq!(
            id.accept_webauthn_assertion(&forged.to_string(), &device.ed25519_public),
            Err(AssertionFault::Key)
        );
        assert_eq!(id.method(), IdentityMethod::Stub);
        assert!(!id.subject().contains(&device.fingerprint()));

        assert!(
            id.accept_webauthn_assertion(&assertion, &device.ed25519_public)
                .is_ok()
        );
        assert_eq!(id.method(), IdentityMethod::Passkey);
        assert_eq!(id.enrollment(), EnrollmentState::AssertionVerified);
        assert_eq!(id.subject(), device.did_key());
        assert_ne!(id.subject(), device.fingerprint());
        assert!(id.enrollment_copy().contains("bound to the did:key"));
        assert!(id.unlocked());
        assert_eq!(id.remembered_pin(), Some(verifier.as_str()));
        assert!(verifier.starts_with("pbkdf2-sha256$"));
        id.lock();
        assert!(id.unlock_with_passphrase("alpha"));

        let mut again = LocalIdentityStub::new();
        again.restore_subject(id.subject());
        again
            .restore_passkey_material(Some(challenge), id.passkey_public_key().map(str::to_string));
        again.restore_enrollment(EnrollmentState::AssertionVerified);
        assert_eq!(again.method(), IdentityMethod::Passkey);
        assert_eq!(again.subject(), device.did_key());
    }
}
