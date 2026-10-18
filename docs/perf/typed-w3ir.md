# Typed W3IR + speculative dual-version functions (M1 item 11)

## Before

- `w3ir_aot`'s `EscapePlan` already unboxed values *inside* a function. It
  has a per-function register lattice (Number / Bool / String). Number and
  Bool registers live in native banks (`num_regs` / `bool_regs`). Local
  `let` / `const` / `var` bindings that only ever hold numbers, are not
  captured, and are written before every read live as bare `f64`s
  (`num_bindings`).
- Parameters, `this`, `arguments`, rest parameters and captures were
  *reserved* and always boxed. One untyped parameter in a loop bound or an
  arithmetic chain therefore kept the whole loop on the generic `Value`
  path.
- TypeScript annotations were parsed by the W3IR lowering and then thrown
  away. `ts_transpiler` / `ts_types` map TS types to Rust, but that is the
  separate TS→Rust app path; it never reached W3IR.

## Typed W3IR (part 1, #132)

- `w3cos_ir::types`:
  - `TypeHint` (`Number` / `Bool` / `String` / `Object { name, fields }`).
  - A `FunctionTypes { parameters, returns }` on every `Function`. It is
    serde-default and omitted when empty, so the format is unchanged for
    untyped JS. `FORMAT_VERSION` is now 20.
- The lowering records TS annotations: parameters and return types of
  function declarations and expressions, arrows, methods and constructors.
  - Interfaces (without `extends`), object type aliases and base-class
    declared fields become `Object` hints. Nesting is depth-limited, so
    recursive types terminate.
  - Literal defaults (`x = 0`) count as hints.
  - Optional, rest, destructured, `T | undefined` and mixed-union parameters
    stay `Unknown`.
- `infer(function, speculate)` gives a flow-insensitive register and binding
  type for every value. The lattice is Bottom, Int32, Number, Bool, String,
  Object, Unknown.
  - `Int32` only comes from `| & ^ << >> ~` on numeric operands.
  - `+ - * / %`, unary `-` and `>>>` give `Number`, which covers overflow,
    -0 and NaN.
  - `-0`, fractional and out-of-range constants are `Number`.
  - An operator is typed only when both operands are numeric, because BigInt
    and `valueOf` operands change the result type.
- `speculable_parameters` lists the parameters a backend may speculate on.
  A parameter qualifies only when:
  - it has a hint;
  - it is not captured by a closure;
  - the function has no `arguments` object (sloppy mapped arguments alias
    parameters);
  - the function is sync and not a generator.

## Dual-version lowering (part 2)

For a sync function with Number-hinted, speculable parameters, `w3ir_aot`
builds a second `EscapePlan` that is *seeded* with those parameter
bindings. In that plan:

- each seeded binding holds an `f64` at entry, so it becomes a native
  `num_bindings` slot as long as every later store into it is a number;
- the local copy (`x` for parameter `x`) then usually becomes native too.

The typed variant is emitted only when it unboxes strictly more arithmetic
or comparison instructions than the generic plan. Otherwise the guard and
the extra code would not pay off.

The emitted entry looks like this:

```rust
pub fn f(__this, __args, __captures) -> Value {
    if let (Some(__w3cos_typed_0), Some(__w3cos_typed_2),) =
        (__args.get(0).and_then(Value::as_number), __args.get(2).and_then(Value::as_number),) {
        w3cos_core::typed::record_typed_entry();
        return { /* FTyped frame; num_bindings[k] = __w3cos_typed_0; … run() */ };
    }
    w3cos_core::typed::record_guard_failure();
    /* generic frame, unchanged */
}
```

- **Guard failure.** A string, bool, object, `undefined` or missing
  argument (including one that would trigger a default) runs the generic
  version, with full JS coercions. Speculation is checked only at entry, so
  no mid-function deopt or frame transfer is needed.
- **Number semantics.** Inside the typed version numbers stay `f64`. NaN,
  -0, ±Infinity and 2^53 behave exactly as they do boxed.
- **ToInt32 fix.** The existing numeric fast paths for `| & ^ << >> >>>`
  used Rust `as i32` / `as u32`, which *saturates*: for example
  `4294967301 | 0` gave 2147483647 instead of 5. They now call
  `w3cos_core::intrinsics::f64_to_int32` / `f64_to_uint32` (wrap modulo
  2^32, with an in-range fast path). This bug predates the item. It was
  latent, and parameter speculation exposes it on more code.
- **A/B knob.** `W3COS_AOT_NO_TYPED=1` at compile time turns dual versions
  off.
- **Stats.** `w3cos_core::typed::stats()` counts typed entries and guard
  failures per thread. `leak_ci` prints both.
- **Memory.** Both variants use the same refcounted bindings and captures.
  Every leak-CI scenario still returns to its baseline exactly (zero
  residual).

## Measurements

Box VM (8 vCPU), release builds, `leak_ci` harness.
- *base* = aot-port 19cd07a.
- *generic* = this work with `W3COS_AOT_NO_TYPED=1`. It differs from base
  only by the ToInt32 fix.
- *typed* = this work.
- The numeric rows are the median of 3 interleaved rounds, 20 iterations.
- The scenarios rows are the best of 3–4 runs, 200 iterations; the box is
  noisy (±10%).

| `bench-scenarios/numeric.ts` (ms) | base | generic | typed | speedup vs generic |
|---|---:|---:|---:|---:|
| loop (200k `s += i * step`) | 550 | 538 | **11.9** | 45× |
| mandel (64×48, 50 iters) | 831 | 745 | **4.4** | 169× |
| fib(18) (recursive calls) | 133 | 129 | **96.7** | 1.34× |
| calls (20k `clampMix`) | 333 | 388 | **99.5** | 3.9× |
| nbody (object fields) | 739 | 715 | 718 | 1.0× |
| fannkuch(9) (array elements) | 16.1 | 15.2 | **13.7** | 1.1× |
| guard_failures (10.7% failing calls) | 8.8 | 8.9 | **5.1** | 1.7× |

| `leak-scenarios/scenarios.ts` (ms) | base | generic | typed |
|---|---:|---:|---:|
| realistic_sample | 1587 | 1513 | **1458** (−3.6%, within noise) |
| probe | 297 | 294 | **269** (−8.5%) |
| classes | 240 | 238 | 233 |

### Coverage

| | functions | TS-hinted | speculable | typed version emitted | typed calls | guard failures |
|---|---:|---:|---:|---:|---:|---:|
| scenarios.ts | 46 | 28 (61%) | 11 (24%) | 5 (11%) | 102.8k realistic, 200k probe | 0% |
| numeric.ts | 15 | — | — | 7 (47%) | 571k | 0% except the guard_failures scenario (10.7%) |

`examples/w3ir_type_coverage` reports per-function hints and the share of
typed registers: 55.0% proven, 58.4% speculative on scenarios.ts.

## Not covered yet

- **Known-shape field access by direct slot offset.** `Object` hints are
  recorded, but codegen does not use them. Per-site ICs (#131) already turn
  `obj.x` into a shape compare plus a slot load. Hoisting one shape check per
  function would need an effect analysis proving that no call, store or
  delete in between can change the receiver's shape. The nbody numbers show
  that object-field arithmetic is the next bottleneck: property values come
  back boxed and are never speculated.
- **Speculating property loads, call results or return values.** These would
  need mid-function deopt: a guard inside a block plus a frame transfer into
  the generic state machine. Today speculation happens only at entry.
- **Native `i32` codegen.** Int32 is inferred in the IR, but codegen still
  uses `f64`. The existing loop-counter rewrite already uses `i32` where it
  is proven.
- **Typed calls between AOT functions.** Every call still goes through
  `call_completion` with a `Vec<Value>`. A direct `f64`-ABI call to the typed
  version needs the callee's capture map at the call site.
- **Async functions, generators, functions using `arguments`, Bool/String
  hints.** These are recorded but not specialised.
