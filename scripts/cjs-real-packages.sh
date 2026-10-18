#!/usr/bin/env bash
# Compile real-world CommonJS packages (pinned VS Code dependencies) with the
# w3cos AOT pipeline and compare each entry's console output with Node.
#
#   scripts/cjs-real-packages.sh [entry,entry,...]
#
# Needs node + npm. Packages are installed from the committed lockfile
# (crates/w3cos-compiler/tests/cjs_real/package-lock.json) into a temp dir.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
src="$repo/crates/w3cos-compiler/tests/cjs_real"
work="${W3COS_CJS_REAL_WORK:-$(mktemp -d -t w3cos-cjs-real.XXXXXX)}"
mkdir -p "$work"
cp "$src/package.json" "$src/package-lock.json" "$work/"
rm -rf "$work/src" && cp -R "$src/src" "$work/src"
(cd "$work" && npm ci --ignore-scripts --no-audit --no-fund --silent)
export W3COS_CJS_REAL_DIR="$work"
if [ $# -gt 0 ]; then export W3COS_CJS_REAL_ONLY="$1"; fi
cd "$repo"
cargo test -p w3cos-compiler --lib -- --ignored real_world_commonjs_packages_match_node --nocapture
