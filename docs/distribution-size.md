# Runtime distribution and size budgets

W3COS has one semantic implementation but two distribution models. The OS-linked
model installs the runtime once and packages applications as AOT business code,
metadata, resources, and versioned runtime references. The standalone model
statically carries only the capabilities selected by the application build graph.
Ordinary AOT artifacts must not contain SWC, the compiler, or W3VM; dynamic
browser targets opt into those components through the `dynamic-js` feature.

Initial stripped, uncompressed budgets exclude application-owned assets:

| Profile | Budget | Intended artifact |
| --- | ---: | --- |
| `os-linked-simple` | 1 MiB | Simple application linked to the OS runtime |
| `os-linked-normal` | 5 MiB | Normal application linked to the OS runtime |
| `aot` | 20 MiB | Minimal standalone native AOT UI, per ABI |
| `runtime` | 60 MiB | Standalone runtime with one renderer, per ABI |
| `browser` | 120 MiB | Standalone dynamic browser runtime, per ABI |

Build size-sensitive artifacts with:

```sh
cargo build --profile release-size
```

The `release-size` profile uses size optimization, fat LTO, one codegen unit,
abort-on-panic, and symbol stripping. Feature removal remains the primary size
tool: stripping cannot remove an enabled renderer, decoder, embedded font,
compiler, or VM.

Generated standalone desktop applications embed an equivalent size-oriented
`release` profile and select only the GPU renderer by default. They deliberately
retain `panic = "unwind"` because compiled JavaScript exceptions currently use
Rust unwinding; switching those artifacts to `panic = "abort"` is invalid until
the AOT path represents JavaScript completion records explicitly.

### LogiDesk W3IR reference measurement

The 2026-07-29 arm64 macOS audit reduced the stripped LogiDesk executable from
133,197,312 to 46,075,840 bytes (65.41%). The retained changes are the generated
size profile, single-renderer linkage, erased `JsFunction` closure construction,
W3IR straight-line block coalescing/single-block dispatch removal, grouped
exception capture, explicit raster-codec selection, and text-only clipboard
linkage. Capability-level registration also removes unreferenced
WebGPU/WebGL/WebXR/WebCodecs/ImageDecoder globals from generated applications;
that step reduced the same locked artifact by 150,224 bytes. A separate advanced
media group removes unreferenced Web Audio, media capture/source/session and
WebRTC surfaces while retaining speech recognition; it saved another 265,888
bytes. The compiler restores each complete capability group when an interface is referenced, and
conservatively keeps it for computed access through `window`, `globalThis`,
`self`, or `navigator`. The runtime crate's default feature set remains complete.

The same locked build's link map attributed 10,028,840 bytes to the packaged CJK
face and 736,764 bytes to Inter. Host production paths now select a system UI
font once and preserve the face index for font collections; GPU, CPU/Skia, SVG,
and Harmony rendering no longer package those faces. Fixed font files remain
test-only for deterministic geometry, while application-provided `@font-face`
data remains an application resource. This reduced the prior 56,858,032-byte
artifact by 10,782,192 bytes (18.96%); `__TEXT,__const` alone fell by 10,764,288
bytes.

Several source-level reductions did not improve the linked artifact and are not
part of the implementation: recursive native methods for acyclic CFGs saved
130,480 bytes but increased stack risk; central capture-map helpers increased
the executable by 99,024 bytes; and transitive font/Vello dependency pinning
increased it by 82,464 bytes under the tested resolution. Revisit these only
with a new linker or code-generation strategy and the same locked A/B build.
Binding-load specialization reduced generated Rust by 14.25%, but failed the
real DOM E2E because module initializers can override a local binding through a
capture getter; it was fully reverted rather than weakening W3IR semantics.
Size gates must retain the product's generated `Cargo.lock`; a fresh standalone
resolution is a different dependency graph and is not a valid regression
comparison.

Audit a package using non-overlapping component inputs:

```sh
target/release-size/w3cos-size-audit \
  --profile browser \
  --component executable=dist/browser \
  --component native-library=dist/libwgpu_native.so \
  --component resource=dist/resources.pack \
  --compressed dist/browser-arm64.tar.gz \
  --baseline-bytes 104857600 \
  --max-regression-percent 5 \
  --output target/browser-size.json
```

The JSON report records total and compressed bytes, native-library/resource
attribution, and object text/data/other section sizes. Exit status `2` means the
fixed product budget or regression threshold was exceeded. Inputs must be
non-overlapping: do not provide both a package archive and the files contained
inside it as components.

CI also builds two stripped host linkage probes:

```sh
cargo build -p w3cos-size-probes --bin w3cos-aot-size-probe \
  --profile release-size
cargo build -p w3cos-size-probes --features browser \
  --bin w3cos-browser-size-probe --profile release-size
```

The ordinary probe reaches the real DOM/jsdom runtime without `dynamic-js`. The
browser probe incrementally parses an environment-overridable HTML document,
executes its script-driven DOM mutation through SWC → W3IR → W3VM, and keeps
the network script loader, WOFF/WOFF2 decoder, and configurable persistent-cache path
runtime-reachable through opt-in environment inputs. CI runs both binaries,
audits them against
the `aot` and `browser` budgets, and uploads separate JSON reports. The Browser
report also uses the AOT executable from the same runner as its baseline and
fails if the dynamic linkage increment exceeds 50%. This architecture-local
comparison makes accidental compiler/VM/fetch-cache dependency growth visible
while measuring the actual stripped executable instead of the audit tool itself.

On the current arm64 macOS measurement, the stripped probes are 6,083,264 bytes
for ordinary AOT and 7,148,896 bytes for dynamic Browser linkage: a 1,065,632-byte
(17.52%) incremental cost. The ordinary baseline includes the shared page
`fetch` implementation exposed by both AOT and W3VM; it still excludes the
compiler, W3IR, W3VM, dynamic loader, and WOFF/WOFF2 decoder. These are host
linkage lower bounds, not mobile product-package baselines: neither includes a renderer, packaged fonts,
application assets, ABI packaging overhead, or platform native libraries.

Mobile release gates must audit an Android App Bundle device/ABI split and an iOS
App Store device slice. Universal simulator builds are diagnostic only and must
not become product baselines.

W3COS can produce the unsigned iOS device slice and its build/size receipt
without claiming signing or App Store completion:

```sh
w3cos mobile build path/to/application --platform ios \
  --ios-target device --release \
  --report target/mobile-ios-device.json
```

The report records generation, Native build/package and total elapsed
milliseconds plus the exact executable bytes. Use a fixed Rust toolchain,
generated `Cargo.lock`, target directory and application revision when comparing
reports; simulator and device receipts are intentionally not interchangeable.

The baseline `image` feature set intentionally omits `image/avif`: that feature
links the `ravif`/`rav1e` encoder and does not supply AVIF decoding. A native
embedding with a real AVIF encoding surface can opt in through
`w3cos-runtime/image-avif-encode`; ordinary AOT and mobile applications do not
pay for that encoder chain.
