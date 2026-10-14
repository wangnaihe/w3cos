# L2 agent harness

The shell already resolves exact commands on L0 and synonyms on L1. This
layer is the L2 planner: a free-form request becomes a short sequence of the
existing MCP tools. It is off unless `W3COS_L2` is set.

## What was reused

| Piece | License | Role |
| --- | --- | --- |
| llama.cpp `llama-server` | MIT | Default local runtime. Subprocess. OpenAI-compatible `/v1/chat/completions`, GGUF, CPU. Not linked into the shell. |
| Qwen2.5-Instruct 0.5B and 1.5B, Q4_K_M | Apache-2.0 | Small tool-calling models. Downloaded by `scripts/fetch-l2-models.sh`. Not committed. |
| Ollama | MIT | Optional second process. Same HTTP API (`scripts/w3cos-inferd.sh ollama-0.5b`). |
| Existing `w3cos-ai-mcp` tool list | Apache-2.0 (this repo) | The planner does not grow a second permission model. |

Not linked in:

- **llama-cpp-rs / llama-cpp-2** — same engine, but a C++ build inside the shell binary and a multi-megabyte resident library. The design doc wants inference off the UI process.
- **mistral.rs and candle** — Rust-native, Apache-2.0, and large to compile. Deferred until a GPU or NPU backend is actually needed.
- **Rig, Swiftide, and other agent crates** — they bring their own tool and permission loop. The shell already has confirm, `capability_gate`, and the receipt ledger.

The loop in `agent_loop.rs` is a small ReAct cycle: the model proposes one tool, a read runs immediately, a mutation stops for confirm.

## Gates

Every mutating tool (`propose_intent`, `click`, `type`, and Run) does all of these:

1. The user presses Confirm. Cancel writes a cancelled receipt and runs nothing.
2. The local identity must be unlocked, and the bridge rate limit applies.
3. `AgentPermissions::authorize` must allow the operation. The in-process shell session is `interactive`. The HTTP bridge stays `observer` unless `W3COS_AI_PERMISSIONS` says otherwise.
4. `capability_gate` must already be `granted` for that call. Confirm grants `w3cos-agent-act` for the call, and `w3cos-process` only when the step is the terminal Run button, then revokes both. A direct `confirm` without that grant is denied.
5. A receipt is appended with tier `L2`.

`echo`, `pwd`, and `help` are the only programs Run will spawn. Metacharacters are refused. There is no shell.

Reads (`observe`, `a11y_index`, `query`) need `w3cos-accessibility`, which the shell grants at boot when the session may read the DOM. They do not change the desktop.

An exact L0 command such as `open files` never calls the model. A `#` selector never does either. Multi-clause text (`and`, `then`, `然后`, …) goes to L2 even if the first clause would have matched L0.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `W3COS_L2` | unset | `1` enables the HTTP planner. `script` uses the offline fixture. Anything else is off. |
| `W3COS_L2_BASE_URL` | unset | OpenAI-compatible base, for example `http://127.0.0.1:8081/v1`. Required when `W3COS_L2=1`. |
| `W3COS_L2_MODEL` | `local` | `model` field sent to the server. |
| `W3COS_L2_API_KEY` | unset | Optional bearer token. |
| `W3COS_L2_TIMEOUT_SECS` | 120 | Whole-request timeout. |
| `W3COS_L2_SERVER_PID` | unset | Bench reads this process's `VmRSS`. |

`W3COS_L2=script` is a developer fixture with no weights. It is how the unit suite drives the same confirm path.

The planner thread is named `w3cos-l2`. The UI thread only applies the mailbox. Layout and paint do not call the model.

## Bench

Ten fixed utterances live in `agent_loop::TASKS`. `w3cos-l2-bench` scores the trace (navigation, night mode, terminal output), not a natural-language grade.

```bash
./scripts/fetch-l2-models.sh
./scripts/w3cos-inferd.sh llama-0.5b
export W3COS_L2=1
export W3COS_L2_BASE_URL=http://127.0.0.1:8081/v1
export W3COS_L2_MODEL=qwen2.5-0.5b-instruct
export W3COS_L2_SERVER_PID=$(cat models/inferd.pid)
cargo run -p w3cos-ai-bridge --bin w3cos-l2-bench --release
```

Stop that server before starting `llama-1.5b`. On this 4-core VM the same ten tasks scored:

| combo | pass | median ttft | median step | server RSS | weights | runtime |
| --- | --- | --- | --- | --- | --- | --- |
| llama.cpp b11514 + Qwen2.5-0.5B-Instruct Q4_K_M | 1/10 | 111 ms | 507 ms | 799 MiB | 469 MiB | 44 MiB tree |
| llama.cpp b11514 + Qwen2.5-1.5B-Instruct Q4_K_M | 5/10 | 142 ms | 963 ms | 2.14 GiB | 1.1 GiB | 44 MiB tree |

The 1.5B row is the one to use when someone turns L2 on. It emits `propose_intent` for navigation (files, editor, browser, night mode, and "what is in front"). The 0.5B row is smaller and a little faster, and it does not follow the tool protocol. The shell still ships with `W3COS_L2` unset, so neither model loads until an operator starts `w3cos-inferd.sh` and sets the variables.

Ollama is the same HTTP client (`scripts/w3cos-inferd.sh ollama-0.5b`). It was not installed on the bench machine.

## Deferred

- Standing grants, budgets, and the five-tuple approval broker.
- Streaming tokens into the task center. Steps are appended as each tool finishes. First-token time is measured in the bench.
- Pixel screenshots as model input. `screenshot` returns a note, not PNG bytes.
- Arbitrary process spawn. The allowlist is the whole of `w3cos-process` for this harness.
- Loading and unloading the GGUF from inside the shell. `w3cos-inferd.sh` is the stand-in for a future `w3cos-inferd` daemon.
- mistral.rs, candle, and llama-cpp-2.
