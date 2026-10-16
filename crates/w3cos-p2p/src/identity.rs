//! One 32-byte seed is the device. It expands to the libp2p Ed25519 identity
//! and to the X25519 key the shell already uses for HPKE.

use libp2p::identity::{self, PeerId, ed25519};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

/// Multicodec prefix for a raw Ed25519 public key (`ed25519-pub`).
const ED25519_PUB_PREFIX: [u8; 2] = [0xed, 0x01];

use crate::hex_encode;

pub struct DeviceIdentity {
    pub keypair: identity::Keypair,
    pub peer_id: PeerId,
    pub x25519_secret: [u8; 32],
    pub x25519_public: [u8; 32],
    pub ed25519_public: [u8; 32],
}

impl DeviceIdentity {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let keypair =
            identity::Keypair::ed25519_from_bytes(seed).expect("ed25519 seed is 32 bytes");
        let peer_id = keypair.public().to_peer_id();
        let secret = StaticSecret::from(seed);
        let x25519_public = PublicKey::from(&secret).to_bytes();
        let ed25519 = keypair
            .public()
            .try_into_ed25519()
            .expect("seed produced an ed25519 key");
        Self {
            keypair,
            peer_id,
            x25519_secret: seed,
            x25519_public,
            ed25519_public: ed25519.to_bytes(),
        }
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.x25519_public)
    }

    /// Spec `did:key` for the Ed25519 signing key. This is not the pairing
    /// fingerprint, which is a spelling of the X25519 key.
    pub fn did_key(&self) -> String {
        did_key_ed25519(&self.ed25519_public)
    }

    /// Sign `message` with the same Ed25519 key libp2p uses for this device.
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        self.keypair.sign(message).map_err(|err| err.to_string())
    }
}

/// `did:key` for an Ed25519 public key: `did:key:z` plus base58btc of
/// `0xed 0x01 || public_key`.
pub fn did_key_ed25519(public: &[u8; 32]) -> String {
    let mut payload = Vec::with_capacity(34);
    payload.extend_from_slice(&ED25519_PUB_PREFIX);
    payload.extend_from_slice(public);
    format!("did:key:z{}", bs58::encode(payload).into_string())
}

/// Recover the raw Ed25519 public key from a `did:key` this module produced.
pub fn ed25519_public_from_did(did: &str) -> Option<[u8; 32]> {
    let rest = did.strip_prefix("did:key:z")?;
    if rest.is_empty() {
        return None;
    }
    let bytes = bs58::decode(rest).into_vec().ok()?;
    if bytes.len() != 34 || bytes[0] != ED25519_PUB_PREFIX[0] || bytes[1] != ED25519_PUB_PREFIX[1] {
        return None;
    }
    let mut public = [0u8; 32];
    public.copy_from_slice(&bytes[2..]);
    Some(public)
}

/// Verify an Ed25519 signature from [`DeviceIdentity::sign`].
pub fn verify_ed25519(public: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    let Ok(public) = ed25519::PublicKey::try_from_bytes(public) else {
        return false;
    };
    public.verify(message, signature)
}

/// Same spelling as the shell pairing fingerprint: `did:key:z` plus letters.
pub fn fingerprint(public: &[u8; 32]) -> String {
    let mut out = String::from("did:key:z");
    for byte in public {
        out.push(char::from(b'a' + (byte % 26)));
    }
    out
}

pub fn peer_id_from_seed(seed: [u8; 32]) -> String {
    DeviceIdentity::from_seed(seed).peer_id.to_string()
}

pub fn ed25519_public_from_seed(seed: [u8; 32]) -> [u8; 32] {
    DeviceIdentity::from_seed(seed).ed25519_public
}

pub fn x25519_public_from_seed(seed: [u8; 32]) -> [u8; 32] {
    DeviceIdentity::from_seed(seed).x25519_public
}

/// Nostr uses a different curve. The key is derived so a presence note is
/// stable for this device, and the note still carries the Ed25519 signature
/// the paired peer can check.
pub fn nostr_secret(seed: &[u8; 32]) -> [u8; 32] {
    let mut material = Sha256::new()
        .chain_update(b"w3cos-nostr-v1")
        .chain_update(seed)
        .finalize();
    for tweak in 0u8..32 {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&material);
        bytes[0] ^= tweak;
        if k256::schnorr::SigningKey::from_bytes(&bytes).is_ok() {
            return bytes;
        }
        material = Sha256::digest(bytes);
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&material);
    bytes[31] |= 1;
    bytes
}

pub fn ed25519_hex(bytes: &[u8; 32]) -> String {
    hex_encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_keeps_both_keys() {
        let first = DeviceIdentity::from_seed([7; 32]);
        let second = DeviceIdentity::from_seed([7; 32]);
        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(first.x25519_public, second.x25519_public);
        assert_eq!(first.ed25519_public, second.ed25519_public);
        assert!(first.fingerprint().starts_with("did:key:z"));
        assert_ne!(first.fingerprint(), first.did_key());
        assert_eq!(
            ed25519_public_from_did(&first.did_key()),
            Some(first.ed25519_public)
        );
        let body = b"receipt-body";
        let signature = first.sign(body).unwrap();
        assert!(verify_ed25519(&first.ed25519_public, body, &signature));
        assert!(!verify_ed25519(&first.ed25519_public, b"other", &signature));
        assert_ne!(first.x25519_public, first.ed25519_public);
        let nostr = nostr_secret(&[7; 32]);
        assert_eq!(nostr, nostr_secret(&[7; 32]));
        assert_ne!(nostr, [7; 32]);
    }

    #[test]
    fn did_key_matches_the_ed25519_multicodec_vectors() {
        // W3C CCG did:key example. The bytes are the base58btc payload, not a seed.
        let spec = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
        let public = ed25519_public_from_did(spec).unwrap();
        assert_eq!(
            hex_of(&public),
            "2e6fcce36701dc791488e0d0b1745cc1e33a4c1c9fcc41c63bd343dbbe0970e6"
        );
        assert_eq!(did_key_ed25519(&public), spec);

        // RFC 8032 §7.1 TEST 1 public key, prefixed with ed25519-pub.
        let mut rfc = [0u8; 32];
        for (index, byte) in
            hex_bytes("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
                .iter()
                .enumerate()
        {
            rfc[index] = *byte;
        }
        assert_eq!(
            did_key_ed25519(&rfc),
            "did:key:z6MktwupdmLXVVqTzCw4i46r4uGyosGXRnR3XjN4Zq7oMMsw"
        );
    }

    fn hex_of(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(char::from(HEX[(byte >> 4) as usize]));
            out.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        out
    }

    fn hex_bytes(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }
}
