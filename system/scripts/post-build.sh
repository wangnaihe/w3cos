#!/bin/bash
# Buildroot post-build script
# Copies the W3C OS binary into the root filesystem before image creation.

set -e

TARGET_DIR="$1"
W3COS_BIN="${W3COS_BIN:-/tmp/w3cos-shell}"

if [ -f "$W3COS_BIN" ]; then
    echo "Installing W3C OS Shell to ${TARGET_DIR}/usr/bin/"
    install -m 755 "$W3COS_BIN" "${TARGET_DIR}/usr/bin/w3cos-shell"
else
    echo "WARNING: W3C OS Shell binary not found at $W3COS_BIN"
    echo "Build it first: cargo build --release -p w3cos-cli"
    echo "Then set W3COS_BIN=/path/to/w3cos-shell"
fi
