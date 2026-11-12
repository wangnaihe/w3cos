# Distributing W3COS through npm

Status: **direction accepted by the owner, not implemented.** This document
records decisions for ROADMAP [R4.4](../ROADMAP.md#r44-distribution-and-toolchain).
Nothing here is published yet: `w3cos`, `create-w3cos`, and `@w3cos/*` do not
exist on npm, and the repository has no root `package.json`, release tag, or
publish workflow.

Baseline: `main` @ `4a91b5a`.

## Facts in the code today

These are the constraints the plan has to remove. Each one was checked against
the source, not the README.

- **Consuming npm packages already works for the formal ESM path.** The
  compiler resolves `node_modules` and a subset of `exports`
  (`crates/w3cos-compiler/src/esm_resolver.rs`). CommonJS and third-party UI
  libraries go through an opt-in Vite prebundle: a `vite.config.w3cos.ts` in the
  *parent* of the entry file's directory, with `node_modules/.bin/vite`
  installed there (`prebundle_web_entry` in `crates/w3cos-compiler/src/lib.rs`).
  That location rule is implicit and is one of the items below.
- **`w3cos build` needs the source tree and a Rust toolchain.** The CLI
  transpiles TSX into a temporary Cargo project at `$TMPDIR/w3cos-build`,
  deleting it first, and runs `cargo build` there
  (`crates/w3cos-cli/src/main.rs`). The generated `Cargo.toml` uses
  `path = "<workspace>/crates/w3cos-runtime"` and `.../w3cos-std`
  (`generate_cargo_toml` in `crates/w3cos-compiler/src/codegen.rs`).
- **How the workspace is found.** `find_workspace_root()` walks up to ten
  levels from the CLI executable, then from the current directory, and accepts
  the first `Cargo.toml` that contains `[workspace]`. It does not check that the
  workspace is W3COS. A CLI installed outside the repo only works when it is run
  from inside a checkout, and a user project that is itself a Cargo workspace
  would be picked up by mistake.
- **Every build is cold.** Because the build directory is wiped each time, its
  `target/` is not reused unless the caller sets `CARGO_TARGET_DIR`.
- **Renderer features.** Desktop apps get `w3cos-runtime` with
  `default-features = false, features = ["gpu"]` (Vello), or `["gpu",
  "devtools"]`, or the list in `W3COS_RUNTIME_FEATURES`. Mobile codegen enables
  `skia` (`skia-safe =0.93.1`, which downloads prebuilt Skia at build time).
  The runtime crate's own `default` includes `skia`, so workspace builds pull it
  in too.
- **The CLI links the runtime** (`w3cos-runtime`, `default-features = false`).
  Whether that leaves dynamic links to fontconfig, ALSA, or X11 has not been
  measured. That decides whether a portable single-file binary is possible.
- **Linux system packages** used by CI: `libfontconfig1-dev libx11-dev
  libxss-dev pkg-config libclang-dev cmake g++ libasound2-dev`
  (`.github/workflows/ci.yml`). CI runs only on `ubuntu-latest`.
- **Types.** Templates and examples import `@w3cos/std` (21 files) and the
  compiler treats it as a built-in module. There is no npm package and no
  `.d.ts` for it, and none for `w3cos/native`.
- **The Rust version is not pinned.** There is no `rust-toolchain.toml` and
  no `rust-version`; CI installs whatever `stable` is that day
  (`dtolnay/rust-toolchain@stable`). PR-1 adds the pin.

## Decisions

### D1 Package names and layout

Follows esbuild, Biome, turbo, SWC, and Rolldown.

| Package | Name | Contents | Why |
|---|---|---|---|
| CLI | **`w3cos`** (unscoped) | JS bin shim (`bin/w3cos`). Pins every platform package with an exact version in `optionalDependencies`. Ships the `w3cos/native` types through `exports["./native"].types`. | Short unscoped name, so `npx w3cos` works. Sources already write `import … from "w3cos/native"`, so installing `w3cos` makes those types resolve. |
| Scaffolder | **`create-w3cos`** (unscoped) | `npm create w3cos@latest`, `pnpm create w3cos`, `yarn create w3cos` | npm's `create` convention needs an unscoped `create-<name>`. |
| Platform binaries | **`@w3cos/cli-<os>-<arch>[-<abi>]`** | One native `w3cos` executable. `package.json` declares `os` / `cpu` / `libc`. | Same shape as `@biomejs/cli-linux-x64-musl` and `@swc/core-*`. The scope groups the platform packages and stops squatting. |
| Standard library types | **`@w3cos/std`** | Types only (`.d.ts`): `Column`, `Row`, `Text`, `Button`, …, plus the `navigator.w3cos` global augmentation. | Sources import `"@w3cos/std"`, so a package with that name needs no tsconfig `paths`. The compiler keeps treating it as built in; nothing runs the package's JS. |
| Org | **`@w3cos`** | Free npm org owned by the repository owner. | One namespace for later packages such as `@w3cos/rn-compat`. |

Platform packages (8). Names follow Node's `process.platform` /
`process.arch`. Linux uses `gnu` / `musl` suffixes (as SWC and napi-rs do);
Windows uses `msvc`.

| Package | Rust target | `os` / `cpu` / `libc` | Build |
|---|---|---|---|
| `@w3cos/cli-darwin-arm64` | `aarch64-apple-darwin` | darwin / arm64 | native on `macos-15` |
| `@w3cos/cli-darwin-x64` | `x86_64-apple-darwin` | darwin / x64 | cross-compiled on arm64 (Intel runners are being retired) |
| `@w3cos/cli-linux-x64-gnu` | `x86_64-unknown-linux-gnu` | linux / x64 / glibc | glibc 2.28 baseline (`cargo-zigbuild` or a `manylinux_2_28` container) |
| `@w3cos/cli-linux-arm64-gnu` | `aarch64-unknown-linux-gnu` | linux / arm64 / glibc | `ubuntu-24.04-arm`, glibc 2.28 baseline |
| `@w3cos/cli-linux-x64-musl` | `x86_64-unknown-linux-musl` | linux / x64 / musl | fully static |
| `@w3cos/cli-linux-arm64-musl` | `aarch64-unknown-linux-musl` | linux / arm64 / musl | fully static |
| `@w3cos/cli-win32-x64-msvc` | `x86_64-pc-windows-msvc` | win32 / x64 | `windows-2025`, static CRT (`+crt-static`) |
| `@w3cos/cli-win32-arm64-msvc` | `aarch64-pc-windows-msvc` | win32 / arm64 | `windows-11-arm` |

Mobile targets are not CLI hosts; see D6.

### D2 How the binary arrives

- **`optionalDependencies` with `os` / `cpu` / `libc`. No `postinstall`
  download.** This works with `--ignore-scripts`, corporate proxies, and
  offline caches, and it is what esbuild, Biome, and SWC do now.
- Shim lookup order: `W3COS_BINARY_PATH` → `require.resolve('@w3cos/cli-…/package.json')`
  → a specific error (for example `--no-optional`, or pnpm installing the wrong
  libc variant) with the command that fixes it.
- **The shim re-checks libc at run time** with
  `process.report.getReport().header.glibcVersionRuntime`. This is the fallback
  when pnpm installs the musl package on a glibc host, or both.
- No GitHub Release download fallback in v1, so there is only one supply-chain
  path. GitHub Releases carry tar/zip archives and `SHA256SUMS` for people who
  do not use npm.

### D3 Versions

- **One version for everything**: `w3cos`, `create-w3cos`, `@w3cos/std`, the
  8 platform packages, the Cargo workspace, and the version embedded in the
  CLI. `w3cos` pins platform packages exactly, never with `^`.
- First release is `0.2.0-alpha.N` on the **`next`** dist-tag. After smoke
  tests pass, the owner promotes it by hand with
  `npm dist-tag add w3cos@<version> latest`. Minor versions before 1.0 may
  break compatibility.
- The CLI embeds the git revision, the Rust toolchain version, and the runtime
  source pin (D5).

### D4 Publishing

- **npm Trusted Publishing over GitHub Actions OIDC.** Workflow
  `publish-npm.yml` has `permissions: id-token: write`, runs on a GitHub-hosted
  runner, and uses Node 24 (Trusted Publishing needs npm ≥ 11.5.1 and
  Node ≥ 22.14). The repository is public, so **provenance is generated
  automatically**.
- **No npm tokens.** The job uses GitHub Environment **`npm-publish`** with the
  owner as required reviewer, and only `v*` tags may deploy to it.
- Each package's trusted publisher on npmjs.com: owner `wangnaihe`, repository
  `w3cos`, workflow `publish-npm.yml`, environment `npm-publish`. After that,
  each package is set to "Require two-factor authentication and disallow
  tokens".
- Order: the 8 platform packages, then `@w3cos/std` and `w3cos`, then
  `create-w3cos`. A failed run can be re-run; versions that already exist are
  skipped.
- v1 publishes straight to `next` and promotes `latest` by hand. npm staged
  publishing can be evaluated once it is stable.
- Binaries also get `actions/attest-build-provenance` attestations, attached
  to the GitHub Release.
- **Trusted Publishing can only be configured for packages that already
  exist**, so the owner publishes a `0.0.0-reserved` placeholder of all 11
  packages once, locally, with 2FA. A script for that will be in the repo; the
  owner runs it.

### D5 Rust and system dependencies: `w3cos doctor`

Until the CLI stops compiling a Cargo project per app, users need Rust and a
native linker. The CLI checks for them and installs them only with consent.

**Commands**

- `w3cos doctor` checks and changes nothing. `--json` for machines.
- `w3cos doctor --fix` prints a plan, one item at a time: what is downloaded,
  where it goes, and roughly how much disk it uses. Each item asks for consent
  separately.
- `--yes` (or `W3COS_ASSUME_YES=1`) answers yes for CI. `--dry-run` only
  prints the plan.
- Anything that needs elevation also needs **`--allow-sudo`** (Unix) or
  **`--allow-admin`** (Windows). `--yes` alone never elevates.
- `w3cos build`, `run`, and `dev` run a fast check first. On a TTY a missing
  item offers to run the fix. Without a TTY they fail and print one copyable
  command.

**Where Rust goes**

- **rustup already present:** reuse it. Run
  `rustup toolchain install <pinned> --profile minimal` and add targets. The
  user's default toolchain is not changed.
- **No rustup:** install an isolated rustup at **`~/.w3cos/rust`**
  (`RUSTUP_HOME` / `CARGO_HOME` point there) with
  `-y --no-modify-path --profile minimal --default-toolchain <pinned>`.
  **PATH and shell rc files are not touched**; w3cos sets the variables only
  for the processes it starts.
- `rustup-init` is verified against the official `.sha256` before it runs.
- The pinned version is `rust-toolchain.toml`, embedded into the CLI at build
  time.

**Runtime sources.** The CLI version is tied to one runtime source. The
default becomes a git dependency at `rev = <embedded rev>` instead of today's
`path = <workspace>/crates/...`. Later the runtime sources can ship as a
release tarball, verified by SHA-256 and cached at `~/.w3cos/src/<version>`, so
builds need neither git nor fast GitHub access. Builds share
`~/.w3cos/target/<version>-<target>` instead of a fresh `target/` per build.

**System dependencies per platform**

- **macOS:** Xcode Command Line Tools. `xcode-select --install` opens a system
  dialog and cannot be silent; doctor prompts and waits. CI images already
  have it.
- **Linux:** read `/etc/os-release` and print the install command for
  apt / dnf / pacman / zypper / apk (fontconfig, X11 / xkbcommon / Wayland
  development packages, pkg-config, cmake, clang / libclang, g++, ALSA). Running
  it needs `--allow-sudo`.
- **Windows:** detect MSVC with `vswhere`. If it is missing, install
  `Microsoft.VisualStudio.2022.BuildTools` through `winget` (VCTools workload,
  Windows SDK, plus the ARM64 toolset on ARM64 hosts). This shows UAC and the
  license link and needs `--allow-admin`. LLVM / clang and CMake also come from
  winget when needed.
- **Node and Vite:** Node is present (w3cos came from npm). If a project has
  `vite.config.w3cos.ts` but no Vite, pick the package manager from the
  lockfile (npm / pnpm / yarn / bun) and run the project's own install.

**Networks in China.** Honour `RUSTUP_DIST_SERVER` / `RUSTUP_UPDATE_ROOT`
(rsproxy, USTC), Cargo sparse registry mirrors, the npm registry setting
(npmmirror), and `SKIA_BINARIES_URL` for prebuilt Skia. Doctor measures
download speed and suggests a mirror when it is slow.

**Auditable and removable.** Every install is recorded in
`~/.w3cos/toolchain.json`. `w3cos toolchain clean` removes only what w3cos
installed. It does not remove system packages or MSVC; for those it prints
uninstall instructions.

### D6 Mobile

Later (Phase 6): `w3cos doctor --target android|ios|harmony`. Rust targets can
be installed automatically. Android SDK / NDK go through `sdkmanager`, which
needs a JDK and the user's own `sdkmanager --licenses`. Xcode can only be
detected and pointed to (App Store or developer site). DevEco / OHOS SDK can
only be detected and pointed to.

## Gaps

1. The CLI depends on the source tree (`find_workspace_root` plus `path`
   dependencies), and the workspace search accepts any Cargo workspace.
2. No doctor or bootstrap; no Windows, macOS, or arm64 CI.
3. The CLI binary may link fontconfig / ALSA dynamically. Measure with `ldd`,
   `otool -L`, and `dumpbin /dependents`.
4. No multi-platform build or publish workflow, no npm package directories, no
   tags.
5. No types for `@w3cos/std` and `w3cos/native`. `w3cos init` does not create
   npm project files. The Vite prebundle location rule is implicit.
6. The resolver lacks parts of Node's rules: nested conditions, `./*`,
   `imports` / `#`, array fallbacks, pnpm realpath, the `browser` object form.
   Hard-coded mappings in `npm_bridge.rs` need a review.
7. No versioned npm compatibility suite or matrix (R4.2).
8. **Building and running W3COS on Windows has never been verified** (PTY, IPC,
   render backends, Skia prebuilt coverage for `aarch64-pc-windows-msvc` and
   musl).

## Phased PRs

About one PR each. Every irreversible external step is in
[Owner-only steps](#owner-only-steps).

**Phase 0 — direction and pinning**

- PR-0: this document and ROADMAP R4.4.
- PR-1: `rust-toolchain.toml`, workspace `rust-version`, CI uses the pin.

**Phase 1 — CLI without the source tree, portable (P0)**

- PR-2: configurable runtime source (`path` for development, `git + rev` by
  default, `src-tarball` reserved; overridable by `W3COS_HOME`, `--sdk`, or
  `w3cos.app.json`). `build.rs` embeds rev, version, and Rust version. Shared
  `~/.w3cos/target`. Exit: a CLI copied outside the repo builds
  `hello/app.tsx` from an empty directory; `check-aot-dependency-boundary.sh`
  passes; a second app's first build is measurably faster.
- PR-3: make the CLI's runtime dependency optional (`build` needs no render
  stack; `run` / `dev` call the generated app); no dynamic system libraries;
  static CRT on Windows. Exit: gnu binary `ldd` shows only libc / libm /
  libpthread / ld; musl is static; macOS `otool -L` shows only system
  frameworks; Windows has no VC runtime DLL. Sizes recorded.
- PR-4: CI matrix on `macos-15`, `windows-2025`, `ubuntu-24.04-arm`,
  `windows-11-arm`: `cargo check -p w3cos-cli` and a `hello` build. Failures are
  recorded as known issues and fixed in separate PRs.

**Phase 2 — bootstrap**

- PR-5: `w3cos doctor` (read-only, `--json`). Snapshot tests on three OSes;
  each missing item prints a fix command and exits non-zero.
- PR-6: `--fix` for Rust (reuse or isolated rustup, checksum, mirrors,
  `toolchain.json`). Exit: on clean ubuntu / alpine containers and clean
  Windows / macOS runners, `doctor --fix --yes` then `build hello` succeed;
  tests assert PATH, profiles, and the default toolchain are unchanged;
  `toolchain clean` restores the machine.
- PR-7: `--fix` for system dependencies (distro package managers with
  `--allow-sudo`, macOS CLT prompt, MSVC / LLVM / CMake via winget with
  `--allow-admin`). Exit: `--yes` alone fails without elevating.
- PR-8: build / run / dev pre-check, Vite install on consent; TTY and non-TTY
  tests.

**Phase 3 — types and scaffolding**

- PR-9: `packages/std` (`@w3cos/std` types) and `w3cos/native` types. Exit:
  `tsc --noEmit` over `examples/*/app.tsx` and `templates/shared/app.tsx` with
  no errors.
- PR-10: `w3cos init` writes `package.json` (`w3cos` and `@w3cos/std` as dev
  dependencies; `dev` / `build` / `run` / `doctor` scripts), `tsconfig.json`,
  `.gitignore`; `--template react` adds Vite and `vite.config.w3cos.ts`.
- PR-11: explicit `bundler` / `viteConfig` in `w3cos.app.json`, keeping the
  current parent-directory rule; `.js` / `.mjs` entries.

**Phase 4 — packaging and publishing (dry run first)**

- PR-12: `release-cli.yml` builds the 8 targets with `SHA256SUMS` and
  attestations. Smoke tests per target; musl in alpine; darwin-x64 under
  Rosetta.
- PR-13: `packages/w3cos`, `packages/create-w3cos`, and a platform package
  generator (like Biome's `generate-packages.mjs`). Exit: `npm pack` installs
  through npm, pnpm, yarn, and bun on all 8 platforms pick only the matching
  package.
- PR-14: `publish-npm.yml` (D4) with `--dry-run`; the real first release waits
  for the owner's environment approval.
- PR-15: end-to-end install tests, rehearsed against a local Verdaccio
  registry before the first release.

**Phase 5 — npm package compatibility (closes R4.2)**

- PR-16: resolver follows Node's rules (nested conditions, `./*`, `imports`,
  array fallbacks, condition order `w3cos` → `import` → `module` → `browser` →
  `default`, pnpm realpath, `browser` object).
- PR-17: review `npm_bridge.rs`; remove or deprecate.
- PR-18: versioned `tests/npm-compat/` suite with lockfiles and a nightly job.
- PR-19: generated `docs/npm-compat.md`; check off R4.2.

**Phase 6 — mobile toolchains**: doctor for Android, iOS, HarmonyOS; `w3cos
mobile build` uses the checks.

Order: PR-0 → PR-1 → PR-2 → PR-3 → PR-4 → PR-5 → PR-6 → PR-7 → PR-9 → PR-10 →
PR-12 → PR-13 → PR-14 → PR-15 (first `next` release) → the rest.
Owner steps U1–U3 can happen any time before PR-14, and earlier is better
because it reserves the names.

## Risks

| Risk | Effect | Mitigation |
|---|---|---|
| MSVC Build Tools: UAC, about 5–7 GB, Visual Studio license terms, `winget` missing on older or LTSC systems, extra ARM64 components | Install fails, or users feel something large was installed behind their back | `--allow-admin` plus explicit consent; show size and license link first; offline installer instructions without winget; detect with `vswhere` and skip if present |
| rustup consent and isolation | Changing the user's environment; conflicts with an existing rustup | Reuse by default without changing the default toolchain; isolated install leaves PATH and profiles alone; `toolchain.json` and `toolchain clean`; `--dry-run` |
| Elevation | Trust; CI hangs | `--yes` never elevates; `--allow-sudo` / `--allow-admin` are explicit; no prompts without a TTY |
| Disk | Rust about 1–1.5 GB, MSVC 5–7 GB, runtime `target/` several GB, prebuilt Skia on the order of 100 MB (all to be measured) | Doctor checks free space first; shared target cache; `w3cos cache clean` |
| Network, especially in China | rustup, crates, Skia prebuilts on GitHub, npm can be slow or fail | Mirror variables; speed test with a hint; resumable downloads with retries; runtime tarball can be mirrored |
| Skia prebuilt coverage (musl, Windows arm64 may be missing) | Mobile or opt-in Skia builds compile Skia from source (slow, needs python / ninja / clang) or fail | Measure in PR-4. Desktop apps already default to `gpu` (Vello); document which targets lack Skia and what degrades |
| W3COS runtime on Windows unverified | The CLI installs but apps do not build | PR-4 surfaces it; small fix PRs; otherwise mark Windows experimental in README |
| glibc baseline | CLI does not start on older distributions | Build against glibc 2.28; musl package as fallback |
| pnpm and the `libc` field | Both variants installed, or the wrong one | Shim checks libc at run time |
| 11 packages, partial publish | Mismatched versions | Fixed order; re-runnable; main package last; promote `latest` only after smoke tests |
| Name squatting | Brand and supply-chain risk | Register the org and placeholders early (U1–U3) |
| Supply chain | A malicious version | OIDC with provenance, environment approval, tokens disallowed, 2FA, build attestations, no `postinstall` |
| Version drift (CLI, runtime source, types) | Generated code does not match the runtime | One version plus embedded rev; CI checks it |
| Claiming "npm compatible" too early | Contradicts the roadmap's evidence rule | Publish only packages and versions the suite passes |

## Owner-only steps

These are irreversible or tied to the owner's accounts. No agent does them
without explicit approval.

- **U1** Create the npm org **`@w3cos`** (free, public) and confirm 2FA on the
  owner's npm account.
- **U2** Publish `0.0.0-reserved` placeholders locally with 2FA for all 11
  packages: `w3cos`, `create-w3cos`, `@w3cos/std`, and the 8
  `@w3cos/cli-*` packages (`npm publish --access public`).
- **U3** Configure the trusted publisher for each of the 11 packages
  (`wangnaihe/w3cos`, `publish-npm.yml`, environment `npm-publish`), then turn
  on "Require two-factor authentication and disallow tokens".
- **U4** Create GitHub Environment **`npm-publish`** with the owner as required
  reviewer, deployable only from `v*` tags.
- **U5** Approve the first `v0.2.0-alpha.1` tag and release; decide on
  promoting `latest` after smoke tests.
- **U6** Confirm the compatibility suite candidates (PR-18) and when the
  downstream application switches from a git submodule to the npm package.
- **U7** (optional) Decide on an Apple Developer ID and a Windows code-signing
  certificate. Binaries installed through npm usually do not trigger
  Gatekeeper or SmartScreen, so v1 can ship unsigned.
