#!/bin/bash
# Buildroot post-build script
# Copies the W3C OS Shell binary + CLI into the root filesystem before image creation.

set -e

TARGET_DIR="$1"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "=== W3C OS post-build ==="

# Install w3cos-shell (desktop environment)
SHELL_BIN="${W3COS_SHELL_BIN:-${PROJECT_ROOT}/target/x86_64-unknown-linux-gnu/release/w3cos-shell}"
if [ -f "$SHELL_BIN" ]; then
    echo "  Installing w3cos-shell → ${TARGET_DIR}/usr/bin/"
    install -m 755 "$SHELL_BIN" "${TARGET_DIR}/usr/bin/w3cos-shell"
else
    echo "  WARNING: w3cos-shell binary not found at $SHELL_BIN"
    echo "  Build: cargo build --release -p w3cos-shell --target x86_64-unknown-linux-gnu"
fi

# Install w3cos CLI (build tool)
CLI_BIN="${W3COS_CLI_BIN:-${PROJECT_ROOT}/target/x86_64-unknown-linux-gnu/release/w3cos}"
if [ -f "$CLI_BIN" ]; then
    echo "  Installing w3cos CLI → ${TARGET_DIR}/usr/bin/"
    install -m 755 "$CLI_BIN" "${TARGET_DIR}/usr/bin/w3cos"
fi

# Create app directories
mkdir -p "${TARGET_DIR}/usr/share/w3cos/apps"
mkdir -p "${TARGET_DIR}/var/log"

# Copy built-in app examples
if [ -d "${PROJECT_ROOT}/examples" ]; then
    echo "  Installing example apps → ${TARGET_DIR}/usr/share/w3cos/apps/"
    cp -r "${PROJECT_ROOT}/examples/"* "${TARGET_DIR}/usr/share/w3cos/apps/" 2>/dev/null || true
fi

echo "=== W3C OS post-build complete ==="
