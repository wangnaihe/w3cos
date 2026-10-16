//! The device Ed25519 key signs receipts. The wallet signs only after confirm.

use std::cell::Cell;
use std::sync::Arc;

use serde_json::{Value, json};
use w3cos_ai_bridge::identity::{IdentityMethod, LocalIdentity, LocalIdentityStub};
use w3cos_ai_bridge::intent::{Decision, IntentTier};
use w3cos_ai_bridge::tool_host::{ChainWallet, DeviceSigner, ToolHost};
use w3cos_p2p::{DeviceIdentity, LocalWallet, ed25519_public_from_did, verify_ed25519};

struct Device {
    identity: DeviceIdentity,
    did: String,
    signs: Arc<Cell<u32>>,
}

impl DeviceSigner for Device {
    fn did(&self) -> &str {
        &self.did
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        self.signs.set(self.signs.get() + 1);
        self.identity.sign(message)
    }
}

struct Wallet {
    inner: LocalWallet,
    signs: Arc<Cell<u32>>,
}

impl ChainWallet for Wallet {
    fn address(&self) -> &str {
        self.inner.address()
    }

    fn sign_personal(&self, message: &str) -> Result<String, String> {
        self.signs.set(self.signs.get() + 1);
        self.inner.sign_personal(message)
    }
}

#[test]
fn the_device_key_signs_the_receipt_and_the_wallet_waits_for_confirm() {
    let seed = [11u8; 32];
    let identity = DeviceIdentity::from_seed(seed);
    let did = identity.did_key();
    assert_ne!(identity.fingerprint(), did);
    let mut stub = LocalIdentityStub::new();
    assert!(stub.install_subject(&did));
    assert_eq!(stub.method(), IdentityMethod::DidKey);
    assert_eq!(stub.subject(), did);

    let device_signs = Arc::new(Cell::new(0));
    let wallet_signs = Arc::new(Cell::new(0));
    let wallet = LocalWallet::from_device_seed(&seed).unwrap();
    let address = wallet.address().to_string();
    let mut host = ToolHost::new(
        Device {
            identity,
            did: did.clone(),
            signs: Arc::clone(&device_signs),
        },
        Wallet {
            inner: wallet,
            signs: Arc::clone(&wallet_signs),
        },
    );
    host.set_locale("en");
    host.set_now_ms(1_760_518_800_000);

    let proposed = host
        .dispatch(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "wallet_sign",
                "arguments": { "message": "pay the invoice", "tier": "l0" }
            }
        }))
        .unwrap();
    let text = proposed["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Waiting for confirm"));
    assert!(!text.contains("0x"));
    assert_eq!(device_signs.get(), 0);
    assert_eq!(wallet_signs.get(), 0);

    let locked = host.confirm().unwrap();
    assert_eq!(locked.decision, Decision::Denied);
    assert_eq!(locked.tier, IntentTier::L2);
    assert!(!locked.outcome.contains("0x"));
    assert_eq!(wallet_signs.get(), 0);
    assert!(!locked.outcome.contains(&address));
    assert!(verify(&did, &locked));

    host.set_unlocked(true);
    host.propose(
        "wallet_sign",
        &json!({ "message": "pay the invoice", "tier": "l0" }),
    )
    .unwrap();
    assert_eq!(wallet_signs.get(), 0);
    let signed = host.confirm().unwrap();
    assert_eq!(signed.decision, Decision::Allowed);
    assert_eq!(signed.actor, did);
    assert_eq!(signed.tier, IntentTier::L2);
    assert_eq!(wallet_signs.get(), 1);
    assert_eq!(device_signs.get(), 2);
    assert!(verify(&did, &signed));

    let outcome: Value = serde_json::from_str(&signed.outcome).unwrap();
    let wallet_sig = outcome["signature"].as_str().unwrap();
    assert_eq!(outcome["address"].as_str(), Some(address.as_str()));
    let check = LocalWallet::from_device_seed(&seed).unwrap();
    assert!(check.verify_personal("pay the invoice", wallet_sig));
    assert!(!check.verify_personal("other", wallet_sig));

    let mut forged = signed.clone();
    forged.outcome.push('x');
    let public = ed25519_public_from_did(&did).unwrap();
    assert!(!verify_ed25519(
        &public,
        &forged.signed_body(),
        &signed.signature_bytes().unwrap()
    ));
}

fn verify(did: &str, receipt: &w3cos_ai_bridge::tool_host::SignedReceipt) -> bool {
    let Some(public) = ed25519_public_from_did(did) else {
        return false;
    };
    let Some(signature) = receipt.signature_bytes() else {
        return false;
    };
    verify_ed25519(&public, &receipt.signed_body(), &signature)
}
