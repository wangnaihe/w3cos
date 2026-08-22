# Raw Web Platform Tests

> Status: first runnable baseline · 2026-08-22
> Upstream: <https://github.com/web-platform-tests/wpt>
> Pinned revision: `fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0`

W3COS has two different kinds of conformance evidence:

- `tests/wpt/indexeddb-subset.json` maps selected upstream cases to adapted
  assertions. It does not execute the upstream WPT files.
- `w3cos-wpt` serves and executes unmodified WPT HTML, the upstream
  `resources/testharness.js`, and explicit reftest references through the
  dynamic document, DOM, layout, paint, and Skia paths.

Neither result is a claim that the full WPT repository passes.

## Prepare the pinned upstream checkout

Keep WPT outside this repository. The runner rejects a checkout whose `HEAD`
does not match the manifest or whose working tree is dirty.

```bash
git clone --filter=blob:none --no-checkout \
  https://github.com/web-platform-tests/wpt ../wpt
git -C ../wpt sparse-checkout init --cone
git -C ../wpt sparse-checkout set \
  resources infrastructure dom/nodes css/CSS2
git -C ../wpt fetch origin fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0
git -C ../wpt checkout --detach fa5393bb9f5f7d41cc16d1aeede1809ccd378ac0
```

## Run the gates

The smoke manifest contains one raw `testharness` case and one raw reftest.
Both pass at the recorded baseline, so this command is fail-closed. The same
command runs as a required CI step:

```bash
cargo run -p w3cos-wpt-runner -- \
  --wpt-root ../wpt \
  --suite tests/wpt/w3cos-smoke.json \
  --artifacts target/wpt-smoke
```

The broader five-case baseline is also fail-closed:

```bash
cargo run -p w3cos-wpt-runner -- \
  --wpt-root ../wpt \
  --suite tests/wpt/w3cos-baseline.json \
  --artifacts target/wpt-baseline
```

The first 2026-08-22 run recorded 2 passing and 3 failing cases. Those failures
were retained as evidence and then closed without expected-result exemptions:

- mixed-ASCII-case HTML attributes now use HTML-namespace ASCII normalization;
- empty `id` presence/equality selectors and Window named access now match;
- block-in-inline collapsible whitespace no longer shifts the opacity group.

The recorded baseline is now 5 passing and 0 failing cases. `--report-only`
remains available for intentionally red discovery manifests, but it is not
used by either current gate.

`results.json` contains suite, case, subtest, and pixel-difference data.
Reftests additionally emit `actual`, `expected`, and red-highlighted `diff`
PNGs.

## Execution model

- The local server returns raw upstream files and replaces only
  `/resources/testharnessreport.js` with a result bridge. The upstream
  `testharness.js` is not adapted.
- Each testharness document and each side of a reftest runs in a separate
  process. A page crash or stale Realm state becomes one case error and cannot
  corrupt later results.
- Reftests use the native DOM-to-component, layout, paint-artifact, and Skia
  replay path at the manifest viewport. The bundled Inter face makes the
  offscreen output deterministic instead of depending on a host font.
- Both fuzzy dimensions are enforced: maximum per-channel difference and
  total differing pixels.

## Current boundary

The first runner supports static HTTP `GET`/`HEAD`, raw HTML testharness cases,
explicit `match`/`mismatch` reftests, fixed viewport size, and WPT fuzzy
allowances. It does not yet implement WPT server handlers, `.sub` expansion,
HTTPS origins, testdriver automation, print/manual tests, automatic metadata
discovery, or the full upstream selection system.

ECMAScript language conformance belongs to a separately pinned Test262 runner.
It is the next corpus milestone; WPT results must not be relabeled as Test262
coverage.
