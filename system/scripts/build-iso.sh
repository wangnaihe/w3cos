#!/bin/bash
# Build the W3C OS bootable ISO image.
#
# This script automates the full build pipeline:
# 1. Cross-compile the W3C OS Shell for x86_64-unknown-linux-gnu
# 2. Download Buildroot (if not present)
# 3. Build the bootable ISO
#
# Prerequisites:
#   - Rust toolchain with x86_64-unknown-linux-gnu target
#   - Build tools: gcc, make, python3, ncurses-dev
#
# Usage:
#   ./system/scripts/build-iso.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="${PROJECT_DIR}/build"
BUILDROOT_VERSION="2024.11.1"
BUILDROOT_DIR="${BUILD_DIR}/buildroot-${BUILDROOT_VERSION}"

echo "=== W3C OS ISO Builder ==="
echo "Project: ${PROJECT_DIR}"
echo ""

# Step 1: Build W3C OS Shell for Linux x86_64
echo "=== Step 1: Building W3C OS Shell ==="
cd "$PROJECT_DIR"

if ! rustup target list --installed | grep -q "x86_64-unknown-linux-gnu"; then
    echo "Adding Linux x86_64 target..."
    rustup target add x86_64-unknown-linux-gnu
fi

# For cross-compilation from macOS, you need a Linux cross-compiler.
# On a Linux host, this just works with: cargo build --release --target x86_64-unknown-linux-gnu
# On macOS, use: cross build --release --target x86_64-unknown-linux-gnu
if [ "$(uname)" = "Darwin" ]; then
    echo "Cross-compiling from macOS requires 'cross' tool."
    echo "Install: cargo install cross"
    echo "Or build on a Linux machine / in Docker."
    echo ""
    echo "Attempting Docker-based build..."
    docker run --rm -v "$PROJECT_DIR":/w3cos -w /w3cos rust:1.94-bookworm \
        cargo build --release -p w3cos-cli
    SHELL_BIN="${PROJECT_DIR}/target/release/w3cos"
else
    cargo build --release -p w3cos-cli
    SHELL_BIN="${PROJECT_DIR}/target/release/w3cos"
fi

echo "Shell binary: ${SHELL_BIN}"
echo ""

# Step 2: Download Buildroot
echo "=== Step 2: Preparing Buildroot ==="
mkdir -p "$BUILD_DIR"

if [ ! -d "$BUILDROOT_DIR" ]; then
    echo "Downloading Buildroot ${BUILDROOT_VERSION}..."
    cd "$BUILD_DIR"
    wget -q "https://buildroot.org/downloads/buildroot-${BUILDROOT_VERSION}.tar.xz"
    tar xf "buildroot-${BUILDROOT_VERSION}.tar.xz"
    rm "buildroot-${BUILDROOT_VERSION}.tar.xz"
fi

# Step 3: Build ISO
echo "=== Step 3: Building ISO ==="
cd "$BUILDROOT_DIR"

export W3COS_BIN="$SHELL_BIN"

cp "${PROJECT_DIR}/system/buildroot/w3cos_x86_64_defconfig" configs/
make w3cos_x86_64_defconfig
make -j$(nproc)

ISO_PATH="${BUILDROOT_DIR}/output/images/rootfs.iso9660"
if [ -f "$ISO_PATH" ]; then
    cp "$ISO_PATH" "${PROJECT_DIR}/w3cos.iso"
    echo ""
    echo "=== Build complete ==="
    echo "ISO: ${PROJECT_DIR}/w3cos.iso"
    echo "Size: $(du -h "${PROJECT_DIR}/w3cos.iso" | cut -f1)"
    echo ""
    echo "Test with QEMU:"
    echo "  qemu-system-x86_64 -cdrom w3cos.iso -m 2G"
else
    echo "ERROR: ISO build failed. Check Buildroot output for errors."
    exit 1
fi
