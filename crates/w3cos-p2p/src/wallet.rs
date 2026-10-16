//! Local secp256k1 key derived from the device seed.
//!
//! The Ed25519 seed is not reused as a scalar. HKDF-SHA256 separates the
//! curves. Signing is EIP-191 `personal_sign` through `alloy-signer-local`.
//! There is no chain id, no RPC provider, and no remote key store.

use std::fmt;

use alloy_signer::{Signer, SignerSync};
use alloy_signer_local::PrivateKeySigner;
use hkdf::Hkdf;
use sha2::Sha256;

const MAX_MESSAGE_BYTES: usize = 4096;

/// In-process wallet. The private key never leaves this value.
pub struct LocalWallet {
    address: String,
    signer: PrivateKeySigner,
}

impl fmt::Debug for LocalWallet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalWallet")
            .field("address", &self.address)
            .finish()
    }
}

impl LocalWallet {
    /// Derive the wallet from the same 32-byte seed as [`crate::DeviceIdentity`].
    pub fn from_device_seed(seed: &[u8; 32]) -> Result<Self, String> {
        let signer = derive_signer(seed)?;
        let address = signer.address().to_string();
        Ok(Self { address, signer })
    }

    /// Checksummed `0x` address. Reading it does not sign.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// `None`. This wallet is not attached to a chain.
    pub fn chain_id(&self) -> Option<u64> {
        Signer::chain_id(&self.signer)
    }

    /// EIP-191 signature, `0x` plus 65 bytes. The caller has already confirmed.
    pub fn sign_personal(&self, message: &str) -> Result<String, String> {
        if message.len() > MAX_MESSAGE_BYTES {
            return Err("message is too long".into());
        }
        let signature = self
            .signer
            .sign_message_sync(message.as_bytes())
            .map_err(|err| err.to_string())?;
        Ok(signature.to_string())
    }

    /// True when `signature_hex` is an EIP-191 signature of `message` by this wallet.
    pub fn verify_personal(&self, message: &str, signature_hex: &str) -> bool {
        let Ok(signature) = signature_hex.parse::<alloy_signer::Signature>() else {
            return false;
        };
        let Ok(recovered) = signature.recover_address_from_msg(message.as_bytes()) else {
            return false;
        };
        recovered.to_string() == self.address
    }
}

fn derive_signer(seed: &[u8; 32]) -> Result<PrivateKeySigner, String> {
    for counter in 0u32..64 {
        let mut info = b"w3cos-wallet-secp256k1-v1".to_vec();
        info.extend_from_slice(&counter.to_be_bytes());
        let hk = Hkdf::<Sha256>::new(Some(b"w3cos-device-v1"), seed);
        let mut okm = [0u8; 32];
        hk.expand(&info, &mut okm)
            .map_err(|err| format!("wallet hkdf: {err}"))?;
        if let Ok(signer) = PrivateKeySigner::from_slice(&okm) {
            return Ok(signer);
        }
    }
    Err("device seed did not yield a secp256k1 key".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_keeps_one_address_and_does_not_pick_a_chain() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("alloy-provider") && !manifest.contains("alloy-transport"),
            "the wallet must not gain an RPC client"
        );
        let first = LocalWallet::from_device_seed(&[7; 32]).unwrap();
        let second = LocalWallet::from_device_seed(&[7; 32]).unwrap();
        assert_eq!(first.address(), second.address());
        assert_eq!(first.address().len(), 42);
        assert!(first.address().starts_with("0x"));
        assert_ne!(
            first.address(),
            LocalWallet::from_device_seed(&[8; 32]).unwrap().address()
        );
        assert_eq!(first.chain_id(), None);
        let debug = format!("{first:?}");
        assert!(debug.contains(first.address()));
        assert!(!debug.contains("070707"));
    }

    #[test]
    fn personal_sign_recovers_the_local_address() {
        let wallet = LocalWallet::from_device_seed(&[9; 32]).unwrap();
        let message = "在本机签名";
        let encoded = wallet.sign_personal(message).unwrap();
        assert!(encoded.starts_with("0x"));
        assert_eq!(encoded.len(), 132);
        assert!(wallet.verify_personal(message, &encoded));
        assert!(!wallet.verify_personal("other", &encoded));
        assert!(wallet.sign_personal(&"x".repeat(4097)).is_err());
    }
}
