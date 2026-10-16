//! Shell-side tool host for the local signing sheet.
//!
//! The window is the only caller of confirm and cancel. A proposal does not
//! sign, and the wallet signature never goes into the receipt line.

use std::cell::RefCell;

use w3cos_ai_bridge::tool_host::{
    ChainWallet, DeviceSigner, SignedReceipt, ToolHost, ToolProposal,
};
use w3cos_p2p::{DeviceIdentity, LocalWallet};

struct Device {
    identity: DeviceIdentity,
    did: String,
}

impl DeviceSigner for Device {
    fn did(&self) -> &str {
        &self.did
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        self.identity.sign(message)
    }
}

struct Wallet {
    inner: LocalWallet,
    address: String,
}

impl ChainWallet for Wallet {
    fn address(&self) -> &str {
        &self.address
    }

    fn sign_personal(&self, message: &str) -> Result<String, String> {
        self.inner.sign_personal(message)
    }
}

thread_local! {
    static HOST: RefCell<Option<ToolHost<Device, Wallet>>> = const { RefCell::new(None) };
}

fn with_host<T>(body: impl FnOnce(&mut ToolHost<Device, Wallet>) -> T) -> Result<T, String> {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let seed = crate::handoff::device_seed();
            let identity = DeviceIdentity::from_seed(seed);
            let did = identity.did_key();
            let wallet = LocalWallet::from_device_seed(&seed)?;
            let address = wallet.address().to_string();
            *slot = Some(ToolHost::new(
                Device { identity, did },
                Wallet {
                    inner: wallet,
                    address,
                },
            ));
        }
        Ok(body(slot.as_mut().expect("sign host")))
    })
}

#[cfg(test)]
pub(crate) fn reset() {
    HOST.with(|slot| *slot.borrow_mut() = None);
}

pub(crate) fn propose_tool(
    name: &str,
    message: &str,
    unlocked: bool,
    locale: &str,
    now_ms: u64,
    next_id: u64,
) -> Result<ToolProposal, String> {
    with_host(|host| {
        host.set_unlocked(unlocked);
        host.set_locale(locale);
        host.set_now_ms(now_ms);
        host.set_next_id(next_id);
        host.propose_tool(name, message).map_err(|error| error.text)
    })?
}

pub(crate) fn propose_wallet_sign(
    message: &str,
    unlocked: bool,
    locale: &str,
    now_ms: u64,
    next_id: u64,
) -> Result<ToolProposal, String> {
    with_host(|host| {
        host.set_unlocked(unlocked);
        host.set_locale(locale);
        host.set_now_ms(now_ms);
        host.set_next_id(next_id);
        host.propose_sign(message).map_err(|error| error.text)
    })?
}

pub(crate) fn confirm(unlocked: bool, locale: &str, now_ms: u64) -> Result<SignedReceipt, String> {
    with_host(|host| {
        host.set_unlocked(unlocked);
        host.set_locale(locale);
        host.set_now_ms(now_ms);
        host.confirm().map_err(|error| error.text)
    })?
}

pub(crate) fn cancel(locale: &str, now_ms: u64) -> Result<SignedReceipt, String> {
    with_host(|host| {
        host.set_locale(locale);
        host.set_now_ms(now_ms);
        host.cancel().map_err(|error| error.text)
    })?
}
