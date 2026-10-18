# Backend B: W3IR → machine code with Cranelift (M1 item 13)

Status: slice 1. Opt-in crate `w3cos-cranelift`; CLI entry behind the
`cranelift` feature: `cargo run -p w3cos-cli --features cranelift -- jit app.js --stats`.

## Why

The ISO ships no rustc. Extensions installed at run time, and code produced by
`new Function` / `eval`, must still end up as machine code (plan §3.1 / §4):
backend B compiles W3IR on the device in milliseconds. The rustc backend
(`w3ir_aot`) stays the reference and the build-time path for bundled code.

## Slice 1 design

* **Unit.** One W3IR module → one `JITModule`; each sync function becomes
  `extern "C" fn(*mut Frame) -> u32`. W3IR blocks map 1:1 to Cranelift blocks;
  `Jump` / `Branch` / `Return` / `Throw` are native control flow.
* **Values.** Registers are boxed `w3cos_core::Value`s in the frame. Every
  operation the runtime defines (property access, calls, `+` on mixed types,
  object/array creation, classes, closures) calls an `extern "C"` helper that
  forwards to `w3cos_core::intrinsics`, so semantics come from the same core
  the rustc backend uses.
* **Numbers.** Registers that `w3cos_ir::types::infer` proves numeric get an
  unboxed `f64` shadow (a Cranelift variable). `+ - * /`, unary `-`, `| & ^`
  and branches on them are native instructions. The boxed copy is refreshed
  only where a helper reads it.
* **Bindings.** `Rc<RefCell<(Value, initialized)>>` cells, the rustc backend's
  representation (TDZ included). Closures receive the same `AotCaptureMap`, so
  rustc- and Cranelift-compiled functions can call each other.
* **Exceptions.** Helpers never unwind into JIT frames. Each runs under
  `catch_unwind` and returns a status (0 normal, 1 JS throw, 2 Rust panic). A
  throw branches to the block's catch/finally target (same rule as the rustc
  backend), or returns to the Rust entry, which rethrows.
* **Fallback.** `async`, generators, private names, `super` access, dynamic
  import and `import.meta` make `compile_module` return `Unsupported`. The
  caller keeps the rustc backend / VM.

## Verification

`examples/cranelift_diff.rs` lowers each case once and runs it under both
backends: Cranelift in process, and rustc through one batch fixture crate.
Outcomes must match. Cases are a micro/bench corpus plus test262 files that
need only an operator-level `assert` prelude and the host globals `w3cos-core`
provides on its own (`Object`, `Array`, `Math`, error classes, ...), passed to
both backends as identical binding captures.

```text
cargo run --profile wpt -p w3cos-cranelift --example cranelift_diff -- \
    --test262 <test262 checkout> \
    --prefix test/language/expressions --prefix test/language/statements --limit 600
```

Slice 1 result (x86_64 box, 2026-10-19):

| | Cranelift (backend B) | rustc (`w3ir_aot`) |
|---|---|---|
| cases run under both | 338 (311 test262 + 27 corpus) | same |
| outcome match | 337 + 1 known divergence, 0 mismatches | |
| compile | 1.2 s total, 3.6 ms/case (in process) | 169.6 s batch, 502 ms/case (deps prebuilt) |
| bench-fib fib(25) | 124 ms | 141 ms |
| bench-loop 5e6 iterations | 997 ms | 403 ms |
| bench-sieve 200000 | 201 ms | 143 ms |
| bench-objects 300000 | 232 ms | 112 ms |

The known divergence (`corpus/tdz`) is a rustc-backend bug: reading a
function-local `let` before its declaration returns normally instead of
throwing `ReferenceError`; backend B throws as the spec requires.
Skipped test262 files: negative / flags / includes (prelude has no harness
includes), async and generator functions, files needing other host globals
(`Symbol` is the top one), and files the lowering rejects.

## Next slices

1. Unboxed booleans and int32, numeric bindings in registers (no cell),
   inline caches for property reads.
2. Generators / async as state machines (same resume-block layout as the
   rustc backend).
3. Wire backend B into the `new Function` / `eval` and extension-install
   paths; on-disk code cache keyed by content hash, W^X code pages.
4. The full test262 runner on backend B (all host globals through the same
   capture map, harness includes).
