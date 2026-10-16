//! HPKE base mode, RFC 9180.
//!
//! Suite: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305
//! (`kem_id` 0x0020, `kdf_id` 0x0001, `aead_id` 0x0003). One-shot seals use
//! sequence number 0, so the AEAD nonce is the base nonce.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

const KEM_ID: u16 = 0x0020;
const KDF_ID: u16 = 0x0001;
const AEAD_ID: u16 = 0x0003;

fn suite_id_kem() -> [u8; 5] {
    let kem = KEM_ID.to_be_bytes();
    [b'K', b'E', b'M', kem[0], kem[1]]
}

fn suite_id_hpke() -> [u8; 10] {
    let kem = KEM_ID.to_be_bytes();
    let kdf = KDF_ID.to_be_bytes();
    let aead = AEAD_ID.to_be_bytes();
    [
        b'H', b'P', b'K', b'E', kem[0], kem[1], kdf[0], kdf[1], aead[0], aead[1],
    ]
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for index in 0..64 {
        ipad[index] ^= block[index];
        opad[index] ^= block[index];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let digest = outer.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    if salt.is_empty() {
        hmac_sha256(&[0u8; 32], ikm)
    } else {
        hmac_sha256(salt, ikm)
    }
}

fn hkdf_expand(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    let mut previous = Vec::new();
    let mut counter = 1u8;
    while out.len() < length {
        let mut message = previous.clone();
        message.extend_from_slice(info);
        message.push(counter);
        previous = hmac_sha256(prk, &message).to_vec();
        out.extend_from_slice(&previous);
        counter = counter.wrapping_add(1);
    }
    out.truncate(length);
    out
}

fn labeled_extract(suite_id: &[u8], salt: &[u8], label: &str, ikm: &[u8]) -> [u8; 32] {
    let mut labeled = b"HPKE-v1".to_vec();
    labeled.extend_from_slice(suite_id);
    labeled.extend_from_slice(label.as_bytes());
    labeled.extend_from_slice(ikm);
    hkdf_extract(salt, &labeled)
}

fn labeled_expand(suite_id: &[u8], prk: &[u8], label: &str, info: &[u8], length: usize) -> Vec<u8> {
    let mut labeled = Vec::new();
    labeled.extend_from_slice(&(length as u16).to_be_bytes());
    labeled.extend_from_slice(b"HPKE-v1");
    labeled.extend_from_slice(suite_id);
    labeled.extend_from_slice(label.as_bytes());
    labeled.extend_from_slice(info);
    hkdf_expand(prk, &labeled, length)
}

fn kem_shared(dh: &[u8], enc: &[u8; 32], recipient_public: &[u8; 32]) -> [u8; 32] {
    let suite = suite_id_kem();
    let mut context = enc.to_vec();
    context.extend_from_slice(recipient_public);
    let prk = labeled_extract(&suite, b"", "eae_prk", dh);
    let secret = labeled_expand(&suite, &prk, "shared_secret", &context, 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&secret);
    out
}

fn key_schedule(shared_secret: &[u8], info: &[u8]) -> ([u8; 32], [u8; 12]) {
    let suite = suite_id_hpke();
    let psk_id_hash = labeled_extract(&suite, b"", "psk_id_hash", b"");
    let info_hash = labeled_extract(&suite, b"", "info_hash", info);
    let mut context = vec![0u8];
    context.extend_from_slice(&psk_id_hash);
    context.extend_from_slice(&info_hash);
    let secret = labeled_extract(&suite, shared_secret, "secret", b"");
    let key = labeled_expand(&suite, &secret, "key", &context, 32);
    let nonce = labeled_expand(&suite, &secret, "base_nonce", &context, 12);
    let mut key_bytes = [0u8; 32];
    let mut nonce_bytes = [0u8; 12];
    key_bytes.copy_from_slice(&key);
    nonce_bytes.copy_from_slice(&nonce);
    (key_bytes, nonce_bytes)
}

/// `HPKE1 || enc(32) || ciphertext+tag`. `info` binds the sender.
pub fn seal(
    recipient_public: &[u8; 32],
    sender_public: &[u8; 32],
    plaintext: &[u8],
    ephemeral_seed: [u8; 32],
) -> Option<Vec<u8>> {
    let secret = StaticSecret::from(ephemeral_seed);
    let enc = PublicKey::from(&secret).to_bytes();
    let dh = secret
        .diffie_hellman(&PublicKey::from(*recipient_public))
        .to_bytes();
    if dh == [0; 32] {
        return None;
    }
    let shared = kem_shared(&dh, &enc, recipient_public);
    let info = info_bytes(sender_public);
    let (key, nonce) = key_schedule(&shared, &info);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let ciphertext = cipher.encrypt(Nonce::from_slice(&nonce), plaintext).ok()?;
    let mut out = Vec::with_capacity(5 + 32 + ciphertext.len());
    out.extend_from_slice(b"HPKE1");
    out.extend_from_slice(&enc);
    out.extend_from_slice(&ciphertext);
    Some(out)
}

pub fn open(
    recipient_secret: &[u8; 32],
    sender_public: &[u8; 32],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    let rest = sealed.strip_prefix(b"HPKE1")?;
    if rest.len() < 32 + 16 {
        return None;
    }
    let (enc_bytes, ciphertext) = rest.split_at(32);
    let enc: [u8; 32] = enc_bytes.try_into().ok()?;
    let secret = StaticSecret::from(*recipient_secret);
    let recipient_public = PublicKey::from(&secret).to_bytes();
    let dh = secret.diffie_hellman(&PublicKey::from(enc)).to_bytes();
    let shared = kem_shared(&dh, &enc, &recipient_public);
    let info = info_bytes(sender_public);
    let (key, nonce) = key_schedule(&shared, &info);
    let cipher = ChaCha20Poly1305::new((&key).into());
    cipher.decrypt(Nonce::from_slice(&nonce), ciphertext).ok()
}

fn info_bytes(sender_public: &[u8; 32]) -> Vec<u8> {
    let mut info = b"w3cos-handoff-v1".to_vec();
    info.extend_from_slice(sender_public);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_mode_roundtrip_and_a_flipped_byte_does_not_open() {
        let recipient = StaticSecret::from([4u8; 32]);
        let recipient_public = PublicKey::from(&recipient).to_bytes();
        let sender_public = PublicKey::from(&StaticSecret::from([9u8; 32])).to_bytes();
        let sealed =
            seal(&recipient_public, &sender_public, b"open files", [7u8; 32]).expect("seal");
        assert!(sealed.starts_with(b"HPKE1"));
        assert!(
            !sealed
                .windows(b"open".len())
                .any(|window| window == b"open")
        );
        let opened = open(&recipient.to_bytes(), &sender_public, &sealed).expect("open");
        assert_eq!(opened, b"open files");
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(open(&recipient.to_bytes(), &sender_public, &tampered).is_none());
        let other = PublicKey::from(&StaticSecret::from([3u8; 32])).to_bytes();
        assert!(open(&recipient.to_bytes(), &other, &sealed).is_none());
    }
}
