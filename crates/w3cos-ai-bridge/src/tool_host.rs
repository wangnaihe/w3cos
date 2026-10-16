//! MCP tool host for the agent harness.
//!
//! `tools/call` only proposes. The model has no confirm tool and no JSON-RPC
//! method that signs. [`ToolHost::confirm`] is the human path: it runs the
//! tool, then appends a receipt signed by the device key. [`ToolHost::cancel`]
//! appends a cancelled receipt and does not run the tool.
//!
//! `wallet_sign` is tier L2. A caller cannot lower that tier. The wallet is
//! not asked to sign until confirm, and only when the identity is unlocked.

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::copy;
use crate::intent::{Decision, IntentTier};

const MAX_MESSAGE_BYTES: usize = 4096;

/// POST a JSON-RPC body and return the response body.
pub type ChainTransport = fn(&str, &str) -> Result<String, String>;

/// Device key that signs receipts. Implementations wrap the P2P Ed25519 key.
pub trait DeviceSigner {
    /// Spec `did:key` for this device's Ed25519 public key.
    fn did(&self) -> &str;
    /// Sign `message`. The bytes are the receipt body, not a hash of it.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String>;
}

/// Local chain key. Implementations must not talk to a network.
pub trait ChainWallet {
    /// Checksummed address. Called only after confirm.
    fn address(&self) -> &str;
    /// EIP-191 signature of `message`. Called only after confirm, while unlocked.
    fn sign_personal(&self, message: &str) -> Result<String, String>;
}

/// What the human is being asked to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProposal {
    pub operation_id: u64,
    pub tool: String,
    pub tier: IntentTier,
    pub summary: String,
    pub impact: String,
}

/// One decided tool call. `signature` is the device key over [`Self::signed_body`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedReceipt {
    pub operation_id: u64,
    pub actor: String,
    pub command: String,
    pub arguments: String,
    pub decision: Decision,
    pub confirmed: bool,
    pub outcome: String,
    pub tier: IntentTier,
    pub recorded_at_ms: u64,
    pub signature: String,
}

impl SignedReceipt {
    /// Canonical JSON the device key signed. The signature is not included.
    pub fn signed_body(&self) -> Vec<u8> {
        let value = json!({
            "actor": self.actor,
            "arguments": self.arguments,
            "command": self.command,
            "confirmed": self.confirmed,
            "decision": self.decision.as_str(),
            "operation_id": self.operation_id,
            "outcome": self.outcome,
            "recorded_at_ms": self.recorded_at_ms,
            "tier": tier_token(self.tier),
        });
        serde_json::to_vec(&value).unwrap_or_default()
    }

    /// Device signature bytes, or `None` when the hex is malformed.
    pub fn signature_bytes(&self) -> Option<Vec<u8>> {
        hex_decode(&self.signature)
    }
}

/// Why a propose, confirm, or cancel did not produce a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    pub message_key: &'static str,
    pub text: String,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl std::error::Error for HostError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolKind {
    DeviceDid,
    WalletAddress,
    WalletSign,
    ChainBalance,
    ChainNonce,
    ChainTx,
}

#[derive(Clone)]
struct Pending {
    id: u64,
    kind: ToolKind,
    arguments: String,
    message: Option<String>,
}

/// In-process MCP host. One pending proposal at a time.
pub struct ToolHost<D, W> {
    device: D,
    wallet: W,
    unlocked: bool,
    locale: String,
    now_ms: u64,
    next_id: u64,
    pending: Option<Pending>,
    receipts: Vec<SignedReceipt>,
    chain_endpoint: String,
    chain_transport: ChainTransport,
}

impl<D, W> ToolHost<D, W>
where
    D: DeviceSigner,
    W: ChainWallet,
{
    /// Locked host. The clock stays at 0 until [`Self::set_now_ms`].
    pub fn new(device: D, wallet: W) -> Self {
        Self {
            device,
            wallet,
            unlocked: false,
            locale: "zh-CN".to_string(),
            now_ms: 0,
            next_id: 1,
            pending: None,
            receipts: Vec::new(),
            chain_endpoint: String::new(),
            chain_transport: crate::chain::post_json,
        }
    }

    /// The endpoint the person saved. Empty means a chain read cannot run.
    pub fn set_chain_endpoint(&mut self, url: &str) {
        self.chain_endpoint = url.trim().to_string();
    }

    /// Replace the HTTP call. Tests use this so a read does not leave the process.
    pub fn set_chain_transport(&mut self, transport: ChainTransport) {
        self.chain_transport = transport;
    }

    /// Unlock or lock. Locking does not drop a pending proposal.
    pub fn set_unlocked(&mut self, unlocked: bool) {
        self.unlocked = unlocked;
    }

    /// `zh-CN` or `en`. Other tags follow the shell: unknown tags use zh-CN.
    pub fn set_locale(&mut self, locale: &str) {
        self.locale = copy::resolve_tag(locale);
    }

    /// Unix milliseconds written into the next receipt.
    pub fn set_now_ms(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    /// Raise the next operation id. Reloaded receipts keep the ids they were signed with.
    pub fn set_next_id(&mut self, next_id: u64) {
        if next_id > self.next_id {
            self.next_id = next_id;
        }
    }

    pub fn pending_id(&self) -> Option<u64> {
        self.pending.as_ref().map(|pending| pending.id)
    }

    pub fn receipts(&self) -> &[SignedReceipt] {
        &self.receipts
    }

    /// Propose `wallet_sign` for `message`. Does not sign.
    pub fn propose_sign(&mut self, message: &str) -> Result<ToolProposal, HostError> {
        self.propose("wallet_sign", &json!({ "message": message }))
    }

    /// Propose one host tool. `message` is used only for `wallet_sign`.
    /// Does not read the wallet and does not sign.
    pub fn propose_tool(&mut self, name: &str, message: &str) -> Result<ToolProposal, HostError> {
        match name {
            "device_did" | "wallet_address" | "chain_balance" | "chain_nonce" => {
                self.propose(name, &json!({}))
            }
            "wallet_sign" => self.propose_sign(message),
            "chain_tx" => self.propose(name, &json!({ "hash": message })),
            _ => Err(self.err_args("host-unknown-tool", &[("name", name)])),
        }
    }

    /// Record a proposal. Does not sign, and does not read the wallet.
    pub fn propose(&mut self, name: &str, arguments: &Value) -> Result<ToolProposal, HostError> {
        if self.pending.is_some() {
            return Err(self.err("host-pending"));
        }
        let kind = self.kind(name)?;
        let (arguments, message) = self.prepare(kind, arguments)?;
        let id = self.next_id;
        self.next_id += 1;
        let proposal = self.proposal(id, kind, message.as_deref());
        self.pending = Some(Pending {
            id,
            kind,
            arguments,
            message,
        });
        Ok(proposal)
    }

    /// Human confirm. Locked identities get a denied receipt and the wallet is not used.
    pub fn confirm(&mut self) -> Result<SignedReceipt, HostError> {
        let Some(pending) = self.pending.clone() else {
            return Err(self.err("host-nothing-pending"));
        };
        let (decision, outcome) = if !self.unlocked {
            (Decision::Denied, self.outcome_text("host-locked"))
        } else {
            match self.execute(&pending) {
                Ok(outcome) => (Decision::Allowed, outcome),
                Err(outcome) => (Decision::Denied, outcome),
            }
        };
        let receipt = self.seal(&pending, decision, true, outcome)?;
        self.pending = None;
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    /// Human cancel. The wallet is not used.
    pub fn cancel(&mut self) -> Result<SignedReceipt, HostError> {
        let Some(pending) = self.pending.clone() else {
            return Err(self.err("host-nothing-pending"));
        };
        let outcome = self.outcome_text("host-cancelled");
        let receipt = self.seal(&pending, Decision::Cancelled, false, outcome)?;
        self.pending = None;
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    /// One MCP JSON-RPC message. `None` means the client sent a notification.
    ///
    /// `tools/call` proposes. There is no tool and no method that confirms.
    pub fn dispatch(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        id.as_ref()?;
        let method = message
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "w3cos-tool-host", "version": "0.1.0" }
            })),
            "tools/list" => Ok(self.tool_list()),
            "tools/call" => match self.call_tool(&params) {
                Ok(body) => Ok(body),
                Err(error) => Ok(json!({
                    "content": [{ "type": "text", "text": error.text }],
                    "isError": true
                })),
            },
            "w3cos/result" => Ok(self.result_view()),
            "ping" => Ok(json!({})),
            _ => Err(copy::line(
                &self.locale,
                "host-unknown-tool",
                &[("name", method)],
            )),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": message }
            }),
        })
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, HostError> {
        let name = params
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| self.err("host-unknown-tool"))?;
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        let proposal = self.propose(name, &arguments)?;
        let text = copy::line(
            &self.locale,
            "host-awaiting",
            &[("summary", proposal.summary.as_str())],
        );
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false
        }))
    }

    fn result_view(&self) -> Value {
        if let Some(pending) = &self.pending {
            let proposal = self.proposal(pending.id, pending.kind, pending.message.as_deref());
            return json!({
                "pending": true,
                "operation_id": pending.id,
                "tool": proposal.tool,
                "tier": tier_token(proposal.tier),
                "summary": proposal.summary
            });
        }
        match self.receipts.last() {
            Some(receipt) => json!({
                "pending": false,
                "receipt": receipt
            }),
            None => json!({ "pending": false, "receipt": Value::Null }),
        }
    }

    fn tool_list(&self) -> Value {
        host_catalog(&self.locale)
    }

    fn kind(&self, name: &str) -> Result<ToolKind, HostError> {
        match name {
            "device_did" => Ok(ToolKind::DeviceDid),
            "wallet_address" => Ok(ToolKind::WalletAddress),
            "wallet_sign" => Ok(ToolKind::WalletSign),
            "chain_balance" => Ok(ToolKind::ChainBalance),
            "chain_nonce" => Ok(ToolKind::ChainNonce),
            "chain_tx" => Ok(ToolKind::ChainTx),
            _ => Err(self.err_args("host-unknown-tool", &[("name", name)])),
        }
    }

    fn prepare(
        &self,
        kind: ToolKind,
        arguments: &Value,
    ) -> Result<(String, Option<String>), HostError> {
        match kind {
            ToolKind::DeviceDid
            | ToolKind::WalletAddress
            | ToolKind::ChainBalance
            | ToolKind::ChainNonce => Ok(("{}".to_string(), None)),
            ToolKind::ChainTx => {
                let raw = arguments
                    .get("hash")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim();
                if !crate::chain::is_tx_hash(raw) {
                    return Err(self.err("host-bad-hash"));
                }
                Ok((json!({ "hash": raw }).to_string(), Some(raw.to_string())))
            }
            ToolKind::WalletSign => {
                let raw = arguments
                    .get("message")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| self.err("host-missing-message"))?;
                let message = raw.trim();
                if message.is_empty() {
                    return Err(self.err("host-missing-message"));
                }
                if message.len() > MAX_MESSAGE_BYTES {
                    return Err(self.err("host-message-long"));
                }
                let arguments = json!({ "message": message }).to_string();
                Ok((arguments, Some(message.to_string())))
            }
        }
    }

    fn proposal(&self, id: u64, kind: ToolKind, message: Option<&str>) -> ToolProposal {
        let (tool, tier, summary_key, impact_key) = match kind {
            ToolKind::DeviceDid => (
                "device_did",
                IntentTier::L0,
                "device-did-summary",
                "device-did-impact",
            ),
            ToolKind::WalletAddress => (
                "wallet_address",
                IntentTier::L0,
                "wallet-address-summary",
                "wallet-address-impact",
            ),
            ToolKind::WalletSign => (
                "wallet_sign",
                IntentTier::L2,
                "wallet-sign-summary",
                "wallet-sign-impact",
            ),
            ToolKind::ChainBalance => (
                "chain_balance",
                IntentTier::L0,
                "chain-balance-summary",
                "chain-balance-impact",
            ),
            ToolKind::ChainNonce => (
                "chain_nonce",
                IntentTier::L0,
                "chain-nonce-summary",
                "chain-nonce-impact",
            ),
            ToolKind::ChainTx => (
                "chain_tx",
                IntentTier::L0,
                "chain-tx-summary",
                "chain-tx-impact",
            ),
        };
        let preview = message.map(preview_message).unwrap_or_default();
        ToolProposal {
            operation_id: id,
            tool: tool.to_string(),
            tier,
            summary: copy::line(&self.locale, summary_key, &[("preview", preview.as_str())]),
            impact: copy::line(&self.locale, impact_key, &[]),
        }
    }

    fn execute(&self, pending: &Pending) -> Result<String, String> {
        match pending.kind {
            ToolKind::DeviceDid => {
                Ok(self.outcome_text_args("host-did", &[("did", self.device.did())]))
            }
            ToolKind::WalletAddress => {
                Ok(self.outcome_text_args("host-address", &[("address", self.wallet.address())]))
            }
            ToolKind::WalletSign => {
                let message = pending.message.as_deref().unwrap_or("");
                match self.wallet.sign_personal(message) {
                    Ok(signature) => Ok(json!({
                        "text": copy::line(&self.locale, "host-signed", &[]),
                        "signature": signature,
                        "address": self.wallet.address(),
                    })
                    .to_string()),
                    Err(_) => Err(self.outcome_text("host-wallet-failed")),
                }
            }
            ToolKind::ChainBalance | ToolKind::ChainNonce | ToolKind::ChainTx => {
                self.execute_chain(pending)
            }
        }
    }

    fn execute_chain(&self, pending: &Pending) -> Result<String, String> {
        let Some(endpoint) = crate::chain::parse_endpoint(&self.chain_endpoint) else {
            return Err(self.outcome_text("host-no-rpc"));
        };
        let kind = match pending.kind {
            ToolKind::ChainBalance => crate::chain::ChainRead::Balance,
            ToolKind::ChainNonce => crate::chain::ChainRead::Nonce,
            ToolKind::ChainTx => crate::chain::ChainRead::TxStatus,
            _ => return Err(self.outcome_text("host-rpc-failed")),
        };
        let hash = pending.message.as_deref().unwrap_or("");
        let body = crate::chain::request_body(kind, self.wallet.address(), hash)
            .map_err(|_| self.outcome_text("host-bad-hash"))?;
        let response = (self.chain_transport)(&endpoint.url, &body)
            .map_err(|_| self.outcome_text("host-rpc-failed"))?;
        let fact = crate::chain::interpret(kind, &response)
            .map_err(|_| self.outcome_text("host-rpc-failed"))?;
        let text =
            match kind {
                crate::chain::ChainRead::Balance => self
                    .outcome_text_args("host-balance", &[("wei", fact.as_deref().unwrap_or("0"))]),
                crate::chain::ChainRead::Nonce => self
                    .outcome_text_args("host-nonce", &[("nonce", fact.as_deref().unwrap_or("0"))]),
                crate::chain::ChainRead::TxStatus => {
                    let status = match fact.as_deref() {
                        Some("ok") => "chain-tx-ok",
                        Some("fail") => "chain-tx-fail",
                        _ => "chain-tx-pending",
                    };
                    let word = copy::line(&self.locale, status, &[]);
                    self.outcome_text_args("host-tx", &[("status", &word)])
                }
            };
        Ok(text)
    }

    fn seal(
        &self,
        pending: &Pending,
        decision: Decision,
        confirmed: bool,
        outcome: String,
    ) -> Result<SignedReceipt, HostError> {
        let unsigned = SignedReceipt {
            operation_id: pending.id,
            actor: self.device.did().to_string(),
            command: tool_name(pending.kind).to_string(),
            arguments: pending.arguments.clone(),
            decision,
            confirmed,
            outcome,
            tier: tool_tier(pending.kind),
            recorded_at_ms: self.now_ms,
            signature: String::new(),
        };
        let body = unsigned.signed_body();
        let signature = self
            .device
            .sign(&body)
            .map_err(|_| self.err("host-device-sign-failed"))?;
        Ok(SignedReceipt {
            signature: hex_encode(&signature),
            ..unsigned
        })
    }

    fn outcome_text(&self, key: &str) -> String {
        json!({ "text": copy::line(&self.locale, key, &[]) }).to_string()
    }

    fn outcome_text_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        json!({ "text": copy::line(&self.locale, key, args) }).to_string()
    }

    fn err(&self, key: &'static str) -> HostError {
        self.err_args(key, &[])
    }

    fn err_args(&self, key: &'static str, args: &[(&str, &str)]) -> HostError {
        HostError {
            message_key: key,
            text: copy::line(&self.locale, key, args),
        }
    }
}

fn is_host_tool(name: &str) -> bool {
    matches!(
        name,
        "device_did"
            | "wallet_address"
            | "wallet_sign"
            | "chain_balance"
            | "chain_nonce"
            | "chain_tx"
    )
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema
    })
}

fn tool_name(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::DeviceDid => "device_did",
        ToolKind::WalletAddress => "wallet_address",
        ToolKind::WalletSign => "wallet_sign",
        ToolKind::ChainBalance => "chain_balance",
        ToolKind::ChainNonce => "chain_nonce",
        ToolKind::ChainTx => "chain_tx",
    }
}

fn tool_tier(kind: ToolKind) -> IntentTier {
    match kind {
        ToolKind::WalletSign => IntentTier::L2,
        ToolKind::DeviceDid
        | ToolKind::WalletAddress
        | ToolKind::ChainBalance
        | ToolKind::ChainNonce
        | ToolKind::ChainTx => IntentTier::L0,
    }
}

fn tier_token(tier: IntentTier) -> &'static str {
    match tier {
        IntentTier::L0 => "l0",
        IntentTier::L1 => "l1",
        IntentTier::L2 => "l2",
        IntentTier::Unspecified => "unspecified",
    }
}

/// Descriptions the stdio process can list without a device key.
pub fn host_catalog(locale: &str) -> Value {
    json!({
        "tools": [
            tool("device_did", &copy::line(locale, "tool-device-did", &[]), json!({})),
            tool("wallet_address", &copy::line(locale, "tool-wallet-address", &[]), json!({})),
            tool(
                "wallet_sign",
                &copy::line(locale, "tool-wallet-sign", &[]),
                json!({
                    "type": "object",
                    "properties": { "message": { "type": "string" } },
                    "required": ["message"]
                })
            ),
            tool("chain_balance", &copy::line(locale, "tool-chain-balance", &[]), json!({})),
            tool("chain_nonce", &copy::line(locale, "tool-chain-nonce", &[]), json!({})),
            tool(
                "chain_tx",
                &copy::line(locale, "tool-chain-tx", &[]),
                json!({
                    "type": "object",
                    "properties": { "hash": { "type": "string" } },
                    "required": ["hash"]
                })
            ),
        ]
    })
}

#[derive(Clone, Debug)]
pub struct HostTicket {
    pub name: String,
    pub summary: String,
    pub impact: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct HostSubmitError {
    pub pending: bool,
    pub message: String,
}

struct HostMail {
    request: Option<HostTicket>,
    applied: bool,
    receipt: Option<SignedReceipt>,
    failure: Option<String>,
}

impl Default for HostMail {
    fn default() -> Self {
        Self {
            request: None,
            applied: false,
            receipt: None,
            failure: None,
        }
    }
}

fn host_mail() -> &'static Mutex<HostMail> {
    static MAIL: OnceLock<Mutex<HostMail>> = OnceLock::new();
    MAIL.get_or_init(|| Mutex::new(HostMail::default()))
}

fn lock_mail() -> std::sync::MutexGuard<'static, HostMail> {
    host_mail().lock().unwrap_or_else(|err| err.into_inner())
}

pub fn reset_host_mail() {
    *lock_mail() = HostMail::default();
}

/// Store one host proposal. Does not read the wallet and does not sign.
pub fn submit_host(body: &str) -> Result<HostTicket, HostSubmitError> {
    let value: Value = serde_json::from_str(body).map_err(|_| HostSubmitError {
        pending: false,
        message: "invalid json".into(),
    })?;
    let name = value
        .get("name")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim();
    let arguments = value.get("arguments").cloned().unwrap_or(Value::Null);
    let message = arguments
        .get("message")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let locale = std::env::var("W3COS_LOCALE").unwrap_or_else(|_| "zh-CN".into());
    if !is_host_tool(name) {
        return Err(HostSubmitError {
            pending: false,
            message: copy::line(&locale, "host-unknown-tool", &[("name", name)]),
        });
    }
    let message = if name == "chain_tx" {
        arguments
            .get("hash")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        message
    };
    if name == "chain_tx" && !crate::chain::is_tx_hash(&message) {
        return Err(HostSubmitError {
            pending: false,
            message: copy::line(&locale, "host-bad-hash", &[]),
        });
    }
    if name == "wallet_sign" && (message.is_empty() || message.len() > MAX_MESSAGE_BYTES) {
        let key = if message.is_empty() {
            "host-missing-message"
        } else {
            "host-message-long"
        };
        return Err(HostSubmitError {
            pending: false,
            message: copy::line(&locale, key, &[]),
        });
    }
    let (summary_key, impact_key) = match name {
        "wallet_address" => ("wallet-address-summary", "wallet-address-impact"),
        "wallet_sign" => ("wallet-sign-summary", "wallet-sign-impact"),
        "chain_balance" => ("chain-balance-summary", "chain-balance-impact"),
        "chain_nonce" => ("chain-nonce-summary", "chain-nonce-impact"),
        "chain_tx" => ("chain-tx-summary", "chain-tx-impact"),
        _ => ("device-did-summary", "device-did-impact"),
    };
    let preview = preview_message(&message);
    let ticket = HostTicket {
        name: name.to_string(),
        summary: copy::line(&locale, summary_key, &[("preview", preview.as_str())]),
        impact: copy::line(&locale, impact_key, &[]),
        message,
    };
    let mut mail = lock_mail();
    if mail.request.is_some() {
        return Err(HostSubmitError {
            pending: true,
            message: copy::line(&locale, "host-pending", &[]),
        });
    }
    mail.receipt = None;
    mail.failure = None;
    mail.applied = false;
    mail.request = Some(ticket.clone());
    Ok(ticket)
}

/// One proposal the UI thread has not given to [`ToolHost`] yet.
pub fn take_unapplied() -> Option<HostTicket> {
    let mut mail = lock_mail();
    if mail.applied {
        return None;
    }
    let ticket = mail.request.clone()?;
    mail.applied = true;
    Some(ticket)
}

pub fn fail_host(message: &str) {
    let mut mail = lock_mail();
    mail.request = None;
    mail.applied = false;
    mail.failure = Some(message.to_string());
}

pub fn publish_host_receipt(receipt: &SignedReceipt) {
    let mut mail = lock_mail();
    if mail.request.is_none() {
        return;
    }
    mail.receipt = Some(receipt.clone());
    mail.request = None;
    mail.applied = false;
}

pub fn host_result_json() -> Value {
    let mail = lock_mail();
    if let Some(request) = &mail.request {
        return json!({
            "pending": true,
            "name": request.name,
            "summary": request.summary
        });
    }
    if let Some(receipt) = &mail.receipt {
        return json!({
            "pending": false,
            "receipt": receipt
        });
    }
    if let Some(failure) = &mail.failure {
        return json!({
            "pending": false,
            "status": 400,
            "body": failure
        });
    }
    let locale = std::env::var("W3COS_LOCALE").unwrap_or_else(|_| "zh-CN".into());
    json!({
        "pending": false,
        "summary": copy::line(&locale, "host-nothing-pending", &[])
    })
}

/// stdio `tools/call` posts `/host/propose` and polls `/host/result`.
/// It never sends a confirm method.
pub fn drive_host_call(
    params: &Value,
    http: &mut dyn FnMut(&crate::mcp::BridgeCall) -> (u16, String),
    pause: &mut dyn FnMut(),
    max_polls: u32,
) -> Result<String, String> {
    let name = params
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let locale = std::env::var("W3COS_LOCALE").unwrap_or_else(|_| "zh-CN".into());
    if !is_host_tool(name) {
        return Err(copy::line(&locale, "host-unknown-tool", &[("name", name)]));
    }
    let propose = crate::mcp::BridgeCall {
        method: "POST",
        path: "/host/propose".into(),
        body: Some(json!({ "name": name, "arguments": arguments }).to_string()),
    };
    let (status, body) = http(&propose);
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}\n{body}"));
    }
    let result_call = crate::mcp::BridgeCall {
        method: "GET",
        path: "/host/result".into(),
        body: None,
    };
    let mut polls = max_polls.max(1);
    loop {
        let (status, body) = http(&result_call);
        if host_result_ready(&body) {
            if (200..300).contains(&status) && !host_result_failed(&body) {
                return Ok(format!("HTTP {status}\n{body}"));
            }
            return Err(format!("HTTP {status}\n{body}"));
        }
        polls = polls.saturating_sub(1);
        if polls == 0 {
            let summary = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("summary")
                        .and_then(|item| item.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            return Ok(copy::line(
                &locale,
                "host-awaiting",
                &[("summary", summary.as_str())],
            ));
        }
        pause();
    }
}

fn host_result_ready(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    value.get("pending").and_then(|item| item.as_bool()) == Some(false)
        && (value.get("receipt").is_some() || value.get("status").is_some())
}

fn host_result_failed(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("status").and_then(|item| item.as_u64()))
        .is_some_and(|status| !(200..300).contains(&status))
}

/// One stdio JSON-RPC message for the external host. There is no confirm method.
pub fn stdio_respond(
    message: &Value,
    http: &mut dyn FnMut(&crate::mcp::BridgeCall) -> (u16, String),
    pause: &mut dyn FnMut(),
    max_polls: u32,
) -> Option<Value> {
    let id = message.get("id").cloned();
    id.as_ref()?;
    let method = message
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let locale = std::env::var("W3COS_LOCALE").unwrap_or_else(|_| "zh-CN".into());
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "w3cos-tool-host", "version": "0.1.0" }
        })),
        "tools/list" => Ok(host_catalog(&locale)),
        "tools/call" => match drive_host_call(&params, http, pause, max_polls) {
            Ok(text) => Ok(json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false
            })),
            Err(text) => Ok(json!({
                "content": [{ "type": "text", "text": text }],
                "isError": true
            })),
        },
        "w3cos/result" => {
            let (status, body) = http(&crate::mcp::BridgeCall {
                method: "GET",
                path: "/host/result".into(),
                body: None,
            });
            Ok(json!({
                "content": [{ "type": "text", "text": format!("HTTP {status}\n{body}") }],
                "isError": !(200..300).contains(&status)
            }))
        }
        "ping" => Ok(json!({})),
        other => Err(copy::line(&locale, "host-unknown-tool", &[("name", other)])),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": message }
        }),
    })
}

fn preview_message(message: &str) -> String {
    let mut chars = message.chars();
    let head: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
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

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let hi = hex_nibble(bytes[index])?;
        let lo = hex_nibble(bytes[index + 1])?;
        out.push((hi << 4) | lo);
        index += 2;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeDevice {
        signs: Cell<u32>,
    }

    impl DeviceSigner for FakeDevice {
        fn did(&self) -> &str {
            "did:key:zFakeDeviceKey"
        }

        fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
            self.signs.set(self.signs.get() + 1);
            let mut out = b"sig:".to_vec();
            out.extend_from_slice(message);
            Ok(out)
        }
    }

    struct FakeWallet {
        signs: Cell<u32>,
        reads: Cell<u32>,
        fail: bool,
    }

    impl ChainWallet for FakeWallet {
        fn address(&self) -> &str {
            self.reads.set(self.reads.get() + 1);
            "0xabc"
        }

        fn sign_personal(&self, message: &str) -> Result<String, String> {
            self.signs.set(self.signs.get() + 1);
            if self.fail {
                return Err("no".into());
            }
            Ok(format!("0xsig:{message}"))
        }
    }

    fn harness(fail: bool) -> ToolHost<FakeDevice, FakeWallet> {
        ToolHost::new(
            FakeDevice {
                signs: Cell::new(0),
            },
            FakeWallet {
                signs: Cell::new(0),
                reads: Cell::new(0),
                fail,
            },
        )
    }

    fn sign_call(message: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "wallet_sign", "arguments": { "message": message, "tier": "l0" } }
        })
    }

    #[test]
    fn a_tool_call_proposes_and_does_not_sign() {
        let mut host = harness(false);
        let response = host.dispatch(&sign_call("hello")).unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert!(text.contains("hello"));
        assert!(!text.contains("0xsig"));
        assert_eq!(host.device.signs.get(), 0);
        assert_eq!(host.wallet.signs.get(), 0);
        assert_eq!(host.wallet.reads.get(), 0);
        assert!(host.receipts().is_empty());
        assert_eq!(host.pending_id(), Some(1));

        let names: Vec<_> = host.tool_list()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "device_did",
                "wallet_address",
                "wallet_sign",
                "chain_balance",
                "chain_nonce",
                "chain_tx"
            ]
        );
        let confirm = json!({"jsonrpc": "2.0", "id": 4, "method": "w3cos/confirm"});
        let refused = host.dispatch(&confirm).unwrap();
        assert!(refused.get("error").is_some());
        assert_eq!(host.wallet.signs.get(), 0);
    }

    #[test]
    fn confirm_signs_only_when_unlocked_and_keeps_l2() {
        let mut host = harness(false);
        host.set_now_ms(1_700_000_000_000);
        host.propose(
            "wallet_sign",
            &json!({"message": "  hello  ", "tier": "l0"}),
        )
        .unwrap();
        let denied = host.confirm().unwrap();
        assert_eq!(denied.decision, Decision::Denied);
        assert!(denied.confirmed);
        assert_eq!(denied.tier, IntentTier::L2);
        assert!(!denied.outcome.contains("0xsig"));
        assert!(!denied.outcome.contains("0xabc"));
        assert_eq!(host.wallet.signs.get(), 0);
        assert_eq!(host.wallet.reads.get(), 0);
        assert_eq!(host.device.signs.get(), 1);
        assert!(denied.signature_bytes().unwrap().starts_with(b"sig:"));

        host.set_unlocked(true);
        let proposal = host
            .propose("wallet_sign", &json!({"message": "hello"}))
            .unwrap();
        assert_eq!(proposal.tier, IntentTier::L2);
        assert_eq!(host.wallet.signs.get(), 0);
        let allowed = host.confirm().unwrap();
        assert_eq!(allowed.decision, Decision::Allowed);
        assert_eq!(allowed.tier, IntentTier::L2);
        assert_eq!(allowed.arguments, "{\"message\":\"hello\"}");
        assert!(allowed.outcome.contains("0xsig:hello"));
        assert!(allowed.outcome.contains("0xabc"));
        assert_eq!(host.wallet.signs.get(), 1);
        assert_eq!(allowed.recorded_at_ms, 1_700_000_000_000);
        let body = allowed.signed_body();
        assert!(
            !body
                .windows(allowed.signature.len())
                .any(|window| { window == allowed.signature.as_bytes() })
        );
    }

    #[test]
    fn cancel_and_a_failed_wallet_do_not_invent_a_signature() {
        let mut host = harness(false);
        host.set_unlocked(true);
        host.propose("wallet_sign", &json!({"message": "hello"}))
            .unwrap();
        let cancelled = host.cancel().unwrap();
        assert_eq!(cancelled.decision, Decision::Cancelled);
        assert!(!cancelled.confirmed);
        assert_eq!(host.wallet.signs.get(), 0);
        assert!(host.confirm().is_err());

        let mut failing = harness(true);
        failing.set_unlocked(true);
        failing
            .propose("wallet_sign", &json!({"message": "hello"}))
            .unwrap();
        let failed = failing.confirm().unwrap();
        assert_eq!(failed.decision, Decision::Denied);
        assert!(!failed.outcome.contains("0xsig"));
        assert_eq!(failing.wallet.signs.get(), 1);
    }

    #[test]
    fn address_and_did_stay_hidden_until_confirm() {
        let mut host = harness(false);
        host.set_locale("en");
        let proposal = host.propose("wallet_address", &Value::Null).unwrap();
        assert_eq!(proposal.tier, IntentTier::L0);
        assert!(!proposal.summary.contains("0xabc"));
        assert_eq!(host.wallet.reads.get(), 0);
        host.set_unlocked(true);
        let receipt = host.confirm().unwrap();
        assert!(receipt.outcome.contains("0xabc"));
        assert_eq!(host.wallet.reads.get(), 1);
        assert_eq!(receipt.tier, IntentTier::L0);

        host.propose("device_did", &json!({})).unwrap();
        let receipt = host.confirm().unwrap();
        assert!(receipt.outcome.contains("did:key:zFakeDeviceKey"));
        assert_eq!(receipt.actor, "did:key:zFakeDeviceKey");
    }

    #[test]
    fn sentences_come_from_both_fluent_packs() {
        let mut host = harness(false);
        host.set_locale("en");
        let english = host
            .propose("wallet_sign", &json!({"message": "pay"}))
            .unwrap();
        assert!(english.summary.contains("Sign"));
        assert!(english.impact.contains("device"));
        host.cancel().unwrap();
        host.set_locale("zh-CN");
        let chinese = host
            .propose("wallet_sign", &json!({"message": "pay"}))
            .unwrap();
        assert!(chinese.summary.contains("签名"));
        assert_ne!(english.summary, chinese.summary);
        assert!(
            host.propose("wallet_sign", &json!({"message": "other"}))
                .is_err()
        );
        assert_eq!(host.wallet.signs.get(), 0);
        host.cancel().unwrap();
        assert!(host.propose("wallet_sign", &json!({})).is_err());
        assert!(
            host.propose("wallet_sign", &json!({"message": "   "}))
                .is_err()
        );
        assert!(
            host.propose("wallet_sign", &json!({"message": "x".repeat(4097)}))
                .is_err()
        );
    }

    #[test]
    fn stdio_propose_does_not_confirm_or_sign() {
        reset_host_mail();
        let first = submit_host(r#"{"name":"device_did","arguments":{}}"#).unwrap();
        assert!(first.summary.contains("did:key") || first.summary.contains("设备"));
        let second = submit_host(r#"{"name":"wallet_address","arguments":{}}"#).unwrap_err();
        assert!(second.pending);
        let ticket = take_unapplied().unwrap();
        assert_eq!(ticket.name, "device_did");
        assert!(take_unapplied().is_none());
        assert_eq!(host_result_json()["pending"], true);

        let mut paths = Vec::new();
        let text = drive_host_call(
            &json!({"name": "wallet_sign", "arguments": {"message": "hello"}}),
            &mut |call| {
                paths.push(call.path.clone());
                if call.path == "/host/propose" {
                    assert!(call.body.as_deref().unwrap_or("").contains("hello"));
                    (200, "{\"pending\":true,\"summary\":\"sign\"}".into())
                } else if paths.len() < 3 {
                    (200, "{\"pending\":true,\"summary\":\"sign\"}".into())
                } else {
                    (
                        200,
                        "{\"pending\":false,\"receipt\":{\"command\":\"wallet_sign\"}}".into(),
                    )
                }
            },
            &mut || {},
            4,
        )
        .unwrap();
        assert_eq!(paths, vec!["/host/propose", "/host/result", "/host/result"]);
        assert!(text.contains("wallet_sign"), "{text}");

        let confirm = json!({"jsonrpc": "2.0", "id": 9, "method": "w3cos/confirm"});
        let response = stdio_respond(
            &confirm,
            &mut |_| panic!("confirm must not call the bridge"),
            &mut || {},
            1,
        )
        .unwrap();
        assert!(response.get("error").is_some(), "{response}");
        reset_host_mail();
    }

    thread_local! {
        static CHAIN_CALLS: std::cell::RefCell<Vec<(String, String)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    fn record_chain(url: &str, body: &str) -> Result<String, String> {
        CHAIN_CALLS.with(|slot| slot.borrow_mut().push((url.to_string(), body.to_string())));
        if body.contains("eth_getBalance") {
            Ok(r#"{"jsonrpc":"2.0","id":1,"result":"0x1"}"#.into())
        } else if body.contains("eth_getTransactionCount") {
            Ok(r#"{"jsonrpc":"2.0","id":1,"result":"0xa"}"#.into())
        } else if body.contains("eth_getTransactionReceipt") {
            Ok(r#"{"jsonrpc":"2.0","id":1,"result":null}"#.into())
        } else {
            Err("unexpected method".into())
        }
    }

    #[test]
    fn a_chain_read_waits_for_confirm_and_has_no_default_endpoint() {
        CHAIN_CALLS.with(|slot| slot.borrow_mut().clear());
        let mut host = harness(false);
        host.set_chain_transport(record_chain);
        host.set_unlocked(true);
        host.propose("chain_balance", &json!({})).unwrap();
        assert!(CHAIN_CALLS.with(|slot| slot.borrow().is_empty()));
        let missing = host.confirm().unwrap();
        assert_eq!(missing.decision, Decision::Denied);
        assert!(missing.outcome.contains("节点") || missing.outcome.contains("endpoint"));
        assert!(CHAIN_CALLS.with(|slot| slot.borrow().is_empty()));
        assert!(!missing.outcome.contains("eth_send"));

        host.set_chain_endpoint("http://127.0.0.1:8545");
        host.set_unlocked(false);
        host.propose("chain_balance", &json!({})).unwrap();
        let locked = host.confirm().unwrap();
        assert_eq!(locked.decision, Decision::Denied);
        assert!(CHAIN_CALLS.with(|slot| slot.borrow().is_empty()));

        host.set_unlocked(true);
        host.propose("chain_balance", &json!({})).unwrap();
        assert!(CHAIN_CALLS.with(|slot| slot.borrow().is_empty()));
        let balance = host.confirm().unwrap();
        assert_eq!(balance.decision, Decision::Allowed);
        assert!(balance.outcome.contains('1'));
        assert!(!balance.outcome.contains("0xsig"));
        let calls = CHAIN_CALLS.with(|slot| slot.borrow().clone());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "http://127.0.0.1:8545");
        assert!(calls[0].1.contains("eth_getBalance"));
        assert!(!calls[0].1.contains("eth_sendRawTransaction"));

        host.propose("chain_nonce", &json!({})).unwrap();
        let nonce = host.confirm().unwrap();
        assert!(nonce.outcome.contains("10"));
        let hash = format!("0x{}", "ab".repeat(32));
        host.propose("chain_tx", &json!({ "hash": hash })).unwrap();
        let pending = host.confirm().unwrap();
        assert_eq!(pending.decision, Decision::Allowed);
        assert!(pending.outcome.contains("还没") || pending.outcome.contains("not"));
        assert!(
            host.propose("chain_tx", &json!({ "hash": "0x11" }))
                .is_err()
        );
        assert_eq!(host.wallet.signs.get(), 0);
    }
}
