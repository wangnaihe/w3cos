//! Local secp256k1 key derived from the device seed.
//!
//! The Ed25519 seed is not reused as a scalar. HKDF-SHA256 separates the
//! curves. Signing is EIP-191 `personal_sign` and a legacy transaction, both
//! through `alloy-signer-local`. The signer has no chain id of its own, no
//! RPC provider, and no remote key store. Broadcasting is not this type's job.

use std::fmt;

use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_network::TxSignerSync;
use alloy_primitives::{Address, Bytes, TxKind, U256};
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

    /// Legacy transaction, EIP-155. Returns `0x` plus the signed RLP.
    /// The caller has already confirmed. This does not broadcast.
    pub fn sign_legacy(
        &self,
        chain_id: u64,
        nonce: u64,
        gas_price: u128,
        gas_limit: u64,
        to: &[u8; 20],
        value: &[u8; 32],
        input: &[u8],
    ) -> Result<String, String> {
        if chain_id == 0 || input.len() > 256 {
            return Err("transaction fields".into());
        }
        let mut tx = TxLegacy {
            chain_id: Some(chain_id),
            nonce,
            gas_price,
            gas_limit,
            to: TxKind::Call(Address::from_slice(to)),
            value: U256::from_be_slice(value),
            input: Bytes::copy_from_slice(input),
        };
        let signature = TxSignerSync::sign_transaction_sync(&self.signer, &mut tx)
            .map_err(|err| err.to_string())?;
        let signed = tx.into_signed(signature);
        let raw = signed.encoded_2718();
        Ok(format!("0x{}", hex_encode(&raw)))
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

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
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

    #[test]
    fn legacy_sign_recovers_the_local_address_and_does_not_broadcast() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("alloy-provider") && !manifest.contains("alloy-transport"),
            "the wallet must not gain an RPC client"
        );
        let wallet = LocalWallet::from_device_seed(&[4; 32]).unwrap();
        let to = [0x11; 20];
        let value = {
            let mut bytes = [0u8; 32];
            bytes[31] = 1;
            bytes
        };
        let first = wallet
            .sign_legacy(1, 7, 1_000_000_000, 21_000, &to, &value, &[])
            .unwrap();
        let second = wallet
            .sign_legacy(1, 7, 1_000_000_000, 21_000, &to, &value, &[])
            .unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("0x"));
        assert!(first.len() > 132);
        let other = wallet
            .sign_legacy(1, 8, 1_000_000_000, 21_000, &to, &value, &[])
            .unwrap();
        assert_ne!(first, other);
        assert!(wallet.chain_id().is_none());
        use alloy_eips::eip2718::Decodable2718;
        let raw = hex_decode(first.strip_prefix("0x").unwrap()).unwrap();
        let mut cursor: &[u8] = &raw;
        let signed = alloy_consensus::Signed::<TxLegacy>::decode_2718(&mut cursor).unwrap();
        let recovered = signed.recover_signer().unwrap();
        assert_eq!(recovered.to_string(), wallet.address());
        assert_eq!(signed.tx().nonce, 7);
        assert_eq!(signed.tx().value, U256::from(1));
    }

    fn hex_decode(text: &str) -> Option<Vec<u8>> {
        if !text.len().is_multiple_of(2) {
            return None;
        }
        let mut out = Vec::with_capacity(text.len() / 2);
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let hi = match bytes[index] {
                b'0'..=b'9' => bytes[index] - b'0',
                b'a'..=b'f' => bytes[index] - b'a' + 10,
                _ => return None,
            };
            let lo = match bytes[index + 1] {
                b'0'..=b'9' => bytes[index + 1] - b'0',
                b'a'..=b'f' => bytes[index + 1] - b'a' + 10,
                _ => return None,
            };
            out.push((hi << 4) | lo);
            index += 2;
        }
        Some(out)
    }
}
