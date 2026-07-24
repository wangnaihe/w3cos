# Web API Capability Matrix

This is the canonical capability inventory for the standard JavaScript surface.
`ROADMAP.md` owns sequencing; this file records where each API is actually
reachable and tested.

Legend: ✅ implemented/covered · ⚠️ partial, host-dependent, or compile-only ·
— unavailable. “Conformance” means compiled ESM coverage, not full web-platform
test-suite compliance.

| API family | Engine | ESM surface | Desktop | Android | iOS | Conformance |
|---|---:|---:|---:|---:|---:|---:|
| `Intl.NumberFormat`, `Intl.DateTimeFormat` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Fetch, `Headers`, `Request`, `Response`, abort signals | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `WebSocket` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| IndexedDB, `IDBKeyRange` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `TextEncoder`, `TextDecoder` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| URI encoding and decoding globals | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `RegExp` and regexp-backed string methods | ⚠️ package-gate subset incl. look-around/backrefs/`v` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `BigInt` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry` | ⚠️ explicit finalizer cleanup + warning | ✅ | ✅ | ✅ | ✅ | ✅ |
| `ArrayBuffer`, `DataView`, typed arrays | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `SharedArrayBuffer`, `Atomics` | ⚠️ non-blocking wait fallback | ✅ | ✅ | ✅ | ✅ | ✅ |
| `Blob`, `File`, `FileReader` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `FormData` and Fetch multipart bodies | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `ImageData`, `Path2D`, `OffscreenCanvas` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `Event`, `CustomEvent`, `EventTarget`, event subclasses | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| DOM constructor identity, `Range`, `Selection` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Resize, mutation, intersection, performance observers | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `Worker`, `SharedWorker`, message ports/channels | ⚠️ echo host | ⚠️ script execution pending | ⚠️ | ⚠️ | ⚠️ | ✅ |
| `EventSource` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `XMLHttpRequest` over Fetch | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Notifications | ✅ | ✅ | ✅ | ⚠️ denied | ⚠️ denied | ✅ |
| `ClipboardItem`, `DataTransfer`, async clipboard | ✅ | ✅ | ✅ | ⚠️ memory fallback | ⚠️ memory fallback | ✅ |
| `crypto.getRandomValues`, `crypto.randomUUID` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Fullscreen | ✅ | ✅ | ✅ | ⚠️ host facade | ⚠️ host facade | ✅ |
| Screen Orientation | ✅ | ✅ | ✅ | ⚠️ host facade | ⚠️ host facade | ✅ |
| `VisualViewport` | ✅ | ✅ | ✅ | ⚠️ keyboard inset pending | ⚠️ keyboard inset pending | ✅ |
| computed style and stylesheet cascade | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| SVG DOM and rendering | ⚠️ retained display/hit trees + cached alpha masks; inherited pointer modes, unpainted shape/text and nested `<use>` shadow geometry, author/anonymous `<use>`, mask/filter-independent hits, and transformed clip paths via usvg/resvg | ⚠️ complex nested clips, per-node animation, and GPU vector path pending | ⚠️ | ⚠️ | ⚠️ | ✅ |
| session cookie store | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Web Speech recognition | ⚠️ iOS engine | ✅ | ⚠️ explicit failure | ⚠️ adapter pending | ⚠️ native engine | ✅ |
| Geolocation | ⚠️ host-injectable | ✅ | ⚠️ explicit unavailable | ⚠️ adapter pending | ⚠️ adapter pending | ✅ |
| MediaDevices and media streams | ⚠️ host-injectable | ✅ | ⚠️ adapter pending | ⚠️ adapter pending | ⚠️ adapter pending | ✅ |
| Web Bluetooth (`navigator.bluetooth`, BLE/GATT subset) | ⚠️ host-injectable | ✅ | — | ⚠️ Android adapter | — | ⚠️ surface tests |

Primary compiled-surface coverage lives in
`crates/w3cos-compiler/src/esm_codegen.rs`
(`generated_bundle_runs_jsdom_globals`). Engine and bridge coverage lives in
the owning crate tests and `crates/w3cos-runtime/tests/w3c_feature_matrix.rs`.
