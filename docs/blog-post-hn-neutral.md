# Hacker News — neutral Show HN text (<4000 chars)

Use as **Text** with title like:
`Show HN: W3C OS – TypeScript and DOM compiled to native code`

---

PASTE BELOW (plain text for HN submit box):

W3C OS is an experiment: use a small subset of TypeScript plus a W3C-style DOM/CSS API to describe UI, then compile to a native executable instead of running a JS engine or shipping a browser.

Pipeline: parse TS to an AST, generate Rust source that calls an in-tree DOM/layout/render stack, then build with rustc/LLVM. Runtime uses Taffy for layout, tiny-skia for raster, winit for windows. Linux-only userland for now; there is also work toward a small bootable image (Buildroot) that drops into a shell.

It is early. Phase 0 is in place (workspace of Rust crates, compiler prototype, basic CSS subset, examples). Many things you would expect from a desktop stack are still missing or partial (reactive state, text input, richer events, GPU path, etc.). I am posting because the compile-to-native-from-DOM-shaped-UI angle seems uncommon and I wanted it documented and reviewable.

Repo with code, architecture notes, and build instructions:
https://github.com/wangnaihe/w3cos

If something in the approach is wrong or redundant compared to prior art, corrections are welcome.

---

Notes for you (do not paste into HN):
- Prefer **URL submit** pointing at GitHub if the README already explains enough; use **Text** only when you need context HN lacks.
- Neutral tone reduces “spam / self-promo” flags; avoid superlatives, sponsor links, and “please star”.
- If the URL was submitted before, HN may attach your click to the existing story and your `submitted` list can stay empty — try a short technical post on your own domain, or add `?` query once (not ideal) or wait and use Ask HN for “feedback on design” with link in comment (follow site norms).
- New accounts often see submissions flagged; building karma via substantive comments helps.
