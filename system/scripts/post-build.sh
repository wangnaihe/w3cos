#!/bin/bash
# Buildroot post-build script
# Copies the W3C OS Shell binary + CLI into the root filesystem before image creation.

set -e

TARGET_DIR="$1"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "=== W3C OS post-build ==="

# Install the desktop process. The product binary dlopens libw3cos_runtime.so.
# The statically linked w3cos-shell is only a fallback when that stub is absent.
SHELL_BIN="${W3COS_SHELL_BIN:-}"
if [ -z "$SHELL_BIN" ]; then
    if [ -f "${PROJECT_ROOT}/target/release/w3cos-shell-linked" ]; then
        SHELL_BIN="${PROJECT_ROOT}/target/release/w3cos-shell-linked"
    elif [ -f "${PROJECT_ROOT}/target/x86_64-unknown-linux-gnu/release/w3cos-shell-linked" ]; then
        SHELL_BIN="${PROJECT_ROOT}/target/x86_64-unknown-linux-gnu/release/w3cos-shell-linked"
    else
        SHELL_BIN="${PROJECT_ROOT}/target/x86_64-unknown-linux-gnu/release/w3cos-shell"
    fi
fi
if [ -f "$SHELL_BIN" ]; then
    echo "  Installing w3cos-shell → ${TARGET_DIR}/usr/bin/ (${SHELL_BIN})"
    install -m 755 "$SHELL_BIN" "${TARGET_DIR}/usr/bin/w3cos-shell"
else
    echo "  WARNING: w3cos-shell binary not found at $SHELL_BIN"
    echo "  Build: scripts/build-shared-runtime.sh && cargo build --release -p w3cos-shell-stub"
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

# System shared runtime, VM (inside the runtime dylib) and the thin host.
# Build first: scripts/build-shared-runtime.sh
LIB_DIR="${W3COS_SHARED_LIB_DIR:-${PROJECT_ROOT}/target/shared}"
install -d "${TARGET_DIR}/usr/lib/w3cos" "${TARGET_DIR}/usr/share/w3cos/toolchain"
if [ -f "${LIB_DIR}/libw3cos_runtime.so" ]; then
    echo "  Installing shared runtime → ${TARGET_DIR}/usr/lib/w3cos/"
    cp -a "${LIB_DIR}/." "${TARGET_DIR}/usr/lib/w3cos/"
fi
HOST_BIN="${W3COS_RUNTIME_HOST_BIN:-${PROJECT_ROOT}/target/release/w3cos-runtime-host}"
if [ -f "$HOST_BIN" ]; then
    echo "  Installing w3cos-runtime-host → ${TARGET_DIR}/usr/bin/"
    install -m 755 "$HOST_BIN" "${TARGET_DIR}/usr/bin/w3cos-runtime-host"
fi
COUNTER_SO="${PROJECT_ROOT}/target/shared/libshared_counter.so"
if [ -f "$COUNTER_SO" ]; then
    echo "  Installing counter plugin → ${TARGET_DIR}/usr/share/w3cos/apps/shared-counter/"
    install -d "${TARGET_DIR}/usr/share/w3cos/apps/shared-counter"
    install -m 755 "$COUNTER_SO" "${TARGET_DIR}/usr/share/w3cos/apps/shared-counter/libshared_counter.so"
fi
# rustc stays in the SDK (system/scripts/package-sdk.sh), not in this rootfs.

echo "=== W3C OS post-build complete ==="
