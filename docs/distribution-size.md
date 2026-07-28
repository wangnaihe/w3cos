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
