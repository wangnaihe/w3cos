//! One 32-byte seed is the device. It expands to the libp2p Ed25519 identity
//! and to the X25519 key the shell already uses for HPKE.

use libp2p::identity::{self, PeerId};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

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
        assert_ne!(first.x25519_public, first.ed25519_public);
        let nostr = nostr_secret(&[7; 32]);
        assert_eq!(nostr, nostr_secret(&[7; 32]));
        assert_ne!(nostr, [7; 32]);
    }
}
