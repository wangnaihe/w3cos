#!/bin/bash
# Assemble the W3C OS SDK. This directory is not copied into the ISO.
# The ISO carries /usr/bin/w3cos (the TypeScript compiler frontend) and
# libw3cos_runtime.so. rustc and cargo live only in this package.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DEST="${1:-${ROOT}/dist/w3cos-sdk}"

if ! command -v rustc >/dev/null 2>&1; then
    echo "rustc is not on PATH; cannot assemble the SDK" >&2
    exit 1
fi

SYSROOT="$(rustc --print sysroot)"
rm -rf "$DEST"
mkdir -p "$DEST/rust"
echo "=== packing rustc sysroot → ${DEST}/rust ==="
cp -a "$SYSROOT/bin" "$SYSROOT/lib" "$DEST/rust/"
if [ -f "${ROOT}/target/release/w3cos" ]; then
    install -m 755 "${ROOT}/target/release/w3cos" "$DEST/w3cos"
fi
cat > "$DEST/README.txt" <<'EOF'
W3C OS SDK

This package is not part of the ISO. The ISO contains:

- /usr/bin/w3cos — TypeScript compiler frontend
- /usr/lib/w3cos/libw3cos_runtime.so — the one system runtime and W3VM
- /usr/bin/w3cos-runtime-host — loads bundles, C plugins, and pages
- /usr/bin/w3cos-shell — process that dlopens the runtime

rustc and cargo are here:

    export PATH="/path/to/w3cos-sdk/rust/bin:${PATH}"

Device images do not grow by a rustc sysroot.
EOF
echo "=== SDK ready at ${DEST} ==="
