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
runtime-reachable through opt-in environment inputs. CI builds both with
`CARGO_PROFILE_RELEASE_SIZE_STRIP=none`, keeps those binaries for symbol
attribution, and measures a `strip --strip-all` copy. `scripts/audit-linkage-sizes.sh`
writes JSON and markdown reports. The Browser report also uses the AOT
executable from the same runner as its baseline and fails if the dynamic
linkage increment exceeds 50%. This architecture-local comparison makes
accidental compiler/VM/fetch-cache dependency growth visible while measuring
the actual stripped executable instead of the audit tool itself.

Each report records executable text, file-backed data, read-only data, bss,
gzip level 9 of the stripped bytes (the download-size proxy), and embedded
font/image containers plus any asset files from `crates/w3cos-runtime/assets`
whose bytes occur in the binary. On the `x86_64-unknown-linux-gnu` probes
below, that embedded scan is 0 bytes: the packaged faces are not in ordinary
AOT or dynamic-browser linkage. Pass `--compressed` when a real package
archive exists; that size is recorded separately from the gzip proxy.

Symbol attribution reads the unstripped symbol table (the same class of
measurement as `cargo bloat`). It reports crate totals, the largest symbols,
and feature buckets. Feature buckets are symbol-table bytes, can overlap, and
are not a second link. A zero means those crates were absent. `tiny-skia`
bytes are left on the crate row because resvg also links that crate; the
`cpu-render` bucket counts `softbuffer` only.

CI fails a probe when its stripped size grows by more than 3% or more than
200 KiB versus the checked-in row for the same rustc host triple in
`tools/size-audit/baselines/linkage-probes.json`. A missing triple is not a
failure on a local run and is a failure in CI (`--require-baseline`), so an
arm64 measurement is never compared with an x86_64 runner. To accept a new
size, rerun on that same triple and review the diff:

```sh
scripts/audit-linkage-sizes.sh --update-baseline --baseline-note "why the size changed"
```

The historical full-runtime Monaco artifact (about 172 MB unstripped / 120 MB
stripped on arm64 macOS) is not a CI gate. It mixed application modules with
every default feature. The reproducible replacements are the per-closure rows
in the baseline file, with per-feature symbol bytes beside them. Render, font,
and image-codec product slices are not given a new absolute baseline here.

On `x86_64-unknown-linux-gnu` with Rust 1.99.1, after `strip --strip-all`, the
checked-in rows are:

| Probe | Stripped bytes | text | data | gzip -9 | Embedded |
| --- | ---: | ---: | ---: | ---: | ---: |
| ordinary AOT | 11,932,232 | 7,758,936 | 3,355,625 | 5,124,056 | 0 |
| dynamic browser | 15,750,744 | 10,236,680 | 4,481,901 | 6,854,785 | 0 |

Data includes read-only data (2,717,457 bytes AOT, 3,646,897 bytes browser).
The same-runner dynamic increment is 3,818,512 bytes (32.00% of the AOT
probe). Symbol attribution assigns 686,785 of the browser symbol bytes to
`dynamic-js` crates and 435,725 to image-codec crates. Those codec bytes are
reported only; codec selection is a separate change. `web-bluetooth` is off
in both probes, and its `btleplug` symbol bucket is 0.

Moving `navigator.w3cos.tray` and `navigator.w3cos.keyring` behind `host-tray`
and `host-keyring` removed them from both probes. The runtime `default`
feature set and `w3cos-shell` still enable both, and the shell also enables
`web-geolocation` and `web-bluetooth`. A generated app uses `default-features = false`, so it
enables them with `W3COS_RUNTIME_FEATURES=host-tray,host-keyring` or by
listing those Cargo features. Without the feature the call rejects
`NotSupportedError`. Measured on this same toolchain and strip, before that
bluetooth code was in the tree, ordinary AOT fell from 12,814,744 to
11,884,064 bytes (930,680 bytes, 7.26%) and its gzip proxy from 5,497,725 to
5,100,001. The browser probe fell from 16,621,992 to 15,694,896 bytes
(927,096 bytes, 5.58%). Calls without the feature reject `NotSupportedError`
and do not install an icon or store a secret. Desktop `web-bluetooth` landing
with the feature still off, and with no `btleplug` symbols, then added 21,376
bytes to ordinary AOT and 23,808 bytes to the browser probe. Desktop
`web-geolocation` landing the same way, still off, added another 26,792 bytes
to ordinary AOT and 32,040 bytes to the browser probe. Those post-merge sizes
are the checked-in rows above.

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
