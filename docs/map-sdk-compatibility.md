# Map SDK compatibility gates

W3COS treats map compatibility as an end-to-end browser workload, not as proof
that individual DOM APIs exist. Every dynamically loaded script must continue
through SWC → W3IR → W3VM and use the page's shared Fetch, cache, Cookie, CORS,
timer and microtask implementations.

## Hermetic CI pre-gate

Run:

```sh
cargo test -p w3cos-runtime --no-default-features \
  --features dynamic-js --test map_sdk_compat
```

The fixture uses a local HTTP server and verifies:

1. A DOM-inserted external bootstrap loads asynchronously and receives one
   `load` event.
2. W3VM bootstrap code creates another script whose URL carries a dotted JSONP
   callback name.
3. The JSONP response calls the live page-window callback with nested map-like
   metadata.
4. The callback injects a secondary chunk, whose `load` handler observes the
   SDK namespace published by that chunk.
5. The published factory creates an instance against a real DOM container.
6. The fetched chunk executes common compressed-loader control flow and
   expressions (`for`, `switch`, updates, compound assignment, comma,
   `typeof`, bitwise and shift operations), plus a hoisted function declaration
   with object/array destructuring, nested defaults, inner rest patterns and a
   final rest parameter, and `for (let ...)` closures with per-iteration cells,
   plus `Array#push`, `Array#map` and `Array#join` member calls through the
   shared Core method semantic. It also verifies repeated block-entry lexical
   cells, hoisted block function declarations, and nested/default/rest
   destructuring declarations. Synchronous `for...of` covers destructured
   array entries, per-iteration closures and Unicode string iteration through
   versioned W3IR and observes their results on the live SDK namespace.
7. No script fetch remains pending after initialization and no alternate
   evaluator, network client or cache is used.

This fixture is deterministic and does not require a vendor API key. It proves
the shape of loader and initialization support, but it is not a substitute for
running the selected vendor SDK.

## Vendor acceptance levels

### Level 1 — loader succeeds

The unmodified vendor bootstrap and every required JSONP, classic-script or ESM
chunk load successfully. Redirect, Cookie, CORS, cache, retry, cancellation and
`load`/`error` behavior must be observable in the shared runtime telemetry.

### Level 2 — SDK initializes

The SDK publishes its documented namespace/factory and creates a map instance
against a real W3COS DOM container. Unsupported JavaScript syntax or Web API
failures must identify the exact source URL and operation.

### Level 3 — map is fully interactive

The map renders visible tiles and passes automated pointer drag, touch pan,
wheel/pinch zoom, resize and high-DPI assertions. Screenshot or pixel evidence
is required. Loader success alone cannot satisfy this level.

Vendor URL, version, API key, security configuration and expected screenshots
belong in protected CI/runtime configuration rather than source control.
