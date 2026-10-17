# Web3 and the agent harness

Product direction: a local AI runtime. People run agents on their own hardware.
Developer experience comes first. Ordinary software shows up as APIs and MCP
tools. Web3 is part of that runtime: keys live on the device, actions are
proposed and confirmed here, and nothing in this path depends on a custodian
or a hosted model platform.

This note is the audit of what the tree does today, the gaps, and the order
to close them. The first slice is in the tree: an MCP tool host whose every
call is propose, confirm, then a receipt signed by the device key, plus a
local signing tool that cannot sign on its own.

## Current state

### Web3

| Piece | Where | What it actually does |
| --- | --- | --- |
| Device seed | `w3cos-shell` handoff, `w3cos-p2p::DeviceIdentity` | One 32-byte seed. It becomes the libp2p Ed25519 identity and the X25519 key used for HPKE. |
| Pairing fingerprint | `w3cos-p2p::fingerprint` | The string `did:key:z` plus one letter per X25519 byte. It is a spelling shared with the shell. It is not a W3C `did:key`. The devices screen shows it beside the spec `did:key`. Stored peer records keep this spelling. |
| Spec `did:key` | `did_key_ed25519` | Ed25519 public key as `did:key:z` + base58btc(`0xed 0x01 \|\| key`). Receipts use this. The pairing screen shows it next to the letter fingerprint. Pairing still matches the letter fingerprint, so existing pairs do not move. |
| Receipt signatures | `DeviceIdentity::sign` | The same Ed25519 key that signs presence cards. |
| Local identity in the shell | `w3cos-ai-bridge::identity` | Lock screen still stores a PBKDF2 verifier. A passkey is installed only after `clientDataJSON` and the Ed25519 assertion verify, and the subject becomes the canonical `did:key` of the device key. The devices screen shows that `did:key` beside the letter fingerprint. Pairing still matches the letter fingerprint. |
| Chain account | `w3cos-p2p::LocalWallet` | secp256k1 key from HKDF of the device seed. EIP-191 `personal_sign` and a legacy transaction. The signer has no chain id and does not broadcast. |
| Chain reads | `w3cos-ai-bridge::chain` | Balance, nonce, and transaction status from an `http` or `https` endpoint the person saved. No built-in endpoint. A permission sheet gates the call. |
| Page credentials | `w3cos-runtime` credentials | Web Credential Management types. The module says wallet and consent flows need a host adapter. They are not wired to the device key. |

### Agent harness

| Piece | Where | What it actually does |
| --- | --- | --- |
| L0 | `intent` | Fixed command table. Propose, confirm, execute, receipt. |
| L1 | `l1`, feature-gated | Synonyms. Still not a model. `W3COS_L1=0` turns it off. |
| L2 | `agent_loop`, `planner`, `agent_host` | Off unless `W3COS_L2` is set. A worker thread calls an OpenAI-compatible HTTP planner (llama.cpp or Ollama, out of process). Every shell tool, including a read, stops for confirm. |
| MCP for the DOM | `mcp`, `w3cos-ai-mcp` | `tools/call` posts `/mcp/propose` and polls `/mcp/result`. Direct HTTP routes do the same: they store one call and wait for the window. |
| In-process tools | `shell_tools` | Same tool names against shell state. The UI thread runs a tool, including a read, only after confirm. |
| Permissions | `permissions`, shell permission sheet | Separate from the confirm card. `w3cos-agent-act` and `w3cos-process` are one-shot. The process allowlist is `echo`, `pwd`, and `help`. |
| Shell receipts | `ActionReceipt`, `ReceiptLedger` | Encrypted file (`W3R1`, ChaCha20-Poly1305). The drawer pages them. They record actor, tier, decision, and time. The device key does not sign these. |
| Harness receipts | `tool_host::SignedReceipt` | In memory. Confirm, deny, and cancel each append one. The device Ed25519 key signs the canonical JSON. |
| Model | `docs/AI_HARNESS.md` | Not linked. `w3cos-inferd.sh` starts llama-server. Weights are not in git. |
| Gated tool host | `tool_host::ToolHost` | `device_did`, `wallet_address`, `wallet_sign`. `tools/call` proposes. Confirm is not a tool and not a JSON-RPC method. |
| Sandbox | `sandbox::run`, shell permission sheet | `echo`, `pwd`, and `help` still use the one-shot process grant. Anything else runs in a new user and mount namespace, with an empty environment. Files and the network stay closed until the sheet grants that one run. The wallet stays in the host process. |

L2 stays off in a normal boot. Confirm does not travel with handoff (#94). Unlock does not travel either.

## Gaps

1. The pairing string looks like a DID and is not one. The devices screen now shows the spec `did:key` of the Ed25519 key beside that string. Pairing still matches the letter fingerprint, and stored peer records are not rewritten.
2. Shell receipts name an actor and are encrypted at rest. They are not signed, so a copied file does not prove which device wrote it.
3. Direct HTTP, the DOM MCP, and the in-process shell tools all propose first. A read does not touch the view until the window confirms. `POST /intent/confirm` is refused. `confirm_intent` is not a tool and `w3cos/confirm` is not a method.
4. There is no local account and no signing tool. There is also no chain client, which should stay true until a later slice asks for one.
5. Passkey enrollment checks `clientDataJSON` and the assertion, then installs the canonical `did:key`. The pairing fingerprint and the P2P seed stay where they were. Old receipt actors are not rewritten.
6. The model cannot be kept in-process without pulling an inference stack into the shell. That split is deliberate and should stay.
7. Anything beyond `echo`, `pwd`, and `help` runs in a separate process. Files and the network stay closed unless the permission sheet grants them for that one run. The device key and the wallet stay in the window process.
8. Host receipts used to live only in memory. The shell sheet now writes them beside the older action rows, and the drawer shows the decision, the `did:key`, and that the device key signed the row. Action receipts written earlier stay unsigned.

## Crate choice

Checked against crates.io in October 2026.

| Crate | Decision | Why |
| --- | --- | --- |
| `rmcp` 3.5.1 (Apache-2.0, MSRV 1.88) | Not linked | Official MCP SDK. Its server runs the tool inside the handler, on tokio. Confirm has to be a person on the UI thread, not another tool the model can call. `mcp.rs` already speaks `initialize`, `tools/list`, and `tools/call`. The new host speaks those same methods and stops before execute. |
| `alloy-signer` and `alloy-signer-local` 2.5 (Apache-2.0) | Linked from `w3cos-p2p` | Local secp256k1 and EIP-191. `k256` was already a dependency. Features that add a mnemonic, a keystore file, or a YubiHSM are off. `alloy-provider` and `alloy-transport` are not dependencies, so this crate cannot open a socket to a chain. AWS, GCP, Turnkey, Ledger, and Trezor signers are not enabled. |
| `did-method-key` 0.5 / `ssi` | Not linked | The method crate pulls `ssi-json-ld`, `ssi-jwk`, and `iref` to encode 34 bytes. |
| `did-key` 0.2.1 | Not linked | Last release 2022. It pins `ed25519-dalek` 1 and mints its own keys. The device key already exists inside libp2p. |
| `bs58` 0.5 | Linked | Base58btc for the `did:key` multibase. The encoder is checked against the W3C CCG example and RFC 8032 TEST 1. |

A `did:key` for this profile is `did:key:z` plus base58btc of the multicodec prefix `0xed 0x01` and the 32-byte Ed25519 public key. That is small enough to live next to `DeviceIdentity`.

## This slice

`ToolHost` in `w3cos-ai-bridge`:

- Tools: `device_did`, `wallet_address`, `wallet_sign`.
- `tools/call` stores one proposal and returns a Fluent sentence. It does not read the address, does not sign, and does not write a receipt.
- `w3cos/confirm` is not a method and confirm is not a tool. `ToolHost::confirm` and `ToolHost::cancel` are the human API.
- Every decision (allow, deny, cancel) appends a receipt. The device Ed25519 key signs the canonical JSON. The signature is not part of that JSON.
- `wallet_sign` is tier L2 even if the arguments ask for L0. The wallet is called only from confirm, and only when the identity is unlocked. A locked confirm writes a denied receipt and does not ask the wallet.
- The wallet key is HKDF-SHA256 of the device seed, info `w3cos-wallet-secp256k1-v1`. It is not the Ed25519 seed. `chain_id` is `None`.
- Sentences are keys in `locales/zh-CN.ftl` and `locales/en.ftl`.

Direct HTTP routes propose first too. `POST /intent/confirm` does not run. `w3cos-tool-host` serves this host on stdio: `tools/call` posts `/host/propose` and polls `/host/result`. The window process is the only caller of confirm.

## Ordered plan

Each slice is one review. Later slices do not start inside this one.

1. **Gated tool host, device-signed receipts, local sign tool.** This change.
2. **Shell sheet.** Done. A permission sheet for `wallet_sign`, separate from navigation confirm and from the process sheet. The receipts drawer shows the decision, the `did:key`, and that the receipt was signed. Strings stay in the Fluent packs. Screenshots are opaque PNG.
3. **Persist host receipts.** Done, in the same change as the sheet. `signed_receipts` is a sibling of `receipts` and defaults to empty on older files. Appending a host receipt does not rewrite action rows.
4. **Move shell tools onto propose-first.** Done. `observe`, `click`, `type`, and `propose_intent` wait for confirm. Reads do not run before that. The existing `w3cos-ai-mcp` `tools/call` proposes into the shell and does not perform the bridge call until the window confirms. Direct HTTP routes do the same. `POST /intent/confirm` is refused. L2 stays off unless `W3COS_L2` is set.
5. **stdio for external agents.** Done. `w3cos-tool-host` speaks MCP on stdio and forwards `tools/call` to `/host/propose`. The process that owns the window is still the only caller of `confirm`. The model does not get that method.
6. **Passkey enrollment.** Done. The window verifies `clientDataJSON` and the Ed25519 assertion before `IdentityMethod::Passkey`, then installs the canonical `did:key` of the device key. Old receipt actors stay as written. The P2P seed and the pairing fingerprint are not replaced.
7. **Read a chain, still no broadcast.** Done. `chain_balance`, `chain_nonce`, and `chain_tx` propose first. The window shows a permission sheet and calls only the `http` or `https` endpoint the person saved. There is no built-in endpoint. Nothing signs or broadcasts.
8. **Sign a transaction.** Done. `wallet_sign_tx` proposes decoded fields. Confirm signs a legacy transaction on the device and writes a receipt that does not contain the signed bytes. A second sheet is the only path to `eth_sendRawTransaction`, and only for that signed transaction, and only to the endpoint the person saved. No custodian and no hosted signer.
9. **Show the spec `did:key` next to the pairing fingerprint.** Done. The devices screen shows the Ed25519 `did:key` beside the letter fingerprint, in one wrapping row. The letter fingerprint stays on the wire and in stored peer records. A peer record with no Ed25519 key does not gain one.
10. **Sandbox untrusted tools.** Done. Anything beyond `echo`, `pwd`, and `help` opens a permission sheet and runs in another process with an empty environment. A user namespace is used when the kernel allows it. Otherwise Landlock keeps files and TCP closed. Either way, files and the network stay closed unless that sheet grants them for the one run. The wallet is not in the sandbox and is still only reached from confirm.
11. **Inference stays out of process.** Keep `w3cos-inferd`. Revisit `mistral.rs`, `candle`, or `llama-cpp-2` only when a GPU or NPU backend is actually linked.

## Out of scope here

- Issue #72 and the size, speed, and PGO profiles.
- Changing pairing fingerprints or the handoff wire format.
- Turning L2 on by default.
- A hosted wallet, a default RPC, or a model-callable confirm.
