#!/usr/bin/env bash
# Build the system runtime cdylib and stable soname paths.
# Ordinary workspace builds stay rlib-only for w3cos-runtime so CI does not link
# a second copy of Skia. This script is the image/toolchain entry point.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export CARGO_PROFILE_RELEASE_LTO="${CARGO_PROFILE_RELEASE_LTO:-off}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-16}"
# The cdylib statically links its rlib dependencies, including std. Rust
# prefer-dynamic cannot do that: crates.io dependencies are not shipped as
# rust dylibs, and the final crate then rejects their rlibs.
export CARGO_PROFILE_RELEASE_PANIC="${CARGO_PROFILE_RELEASE_PANIC:-unwind}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

echo "=== shared runtime cdylib (cpu-render + dynamic-js, LTO off) ==="
cargo rustc -p w3cos-runtime --release --lib --no-default-features --features cpu-render,dynamic-js --crate-type cdylib

DEST="$ROOT/target/shared"
mkdir -p "$DEST"

if [[ ! -f "$ROOT/target/release/libw3cos_runtime.so" ]]; then
    echo "missing target/release/libw3cos_runtime.so" >&2
    exit 1
fi
ln -sfn "$ROOT/target/release/libw3cos_runtime.so" "$DEST/libw3cos_runtime.so"
ln -sfn "$ROOT/target/release/libw3cos_runtime.so" "$DEST/libw3cos_runtime.so.1"

echo "=== shared runtime ready in $DEST ==="
ls -lh "$DEST/libw3cos_runtime.so"
