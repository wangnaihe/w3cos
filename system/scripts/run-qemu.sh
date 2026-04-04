#!/bin/bash
# W3C OS — Quick-start in QEMU virtual machine.
#
# Usage:
#   ./system/scripts/run-qemu.sh                      # Boot from default ISO path
#   ./system/scripts/run-qemu.sh path/to/w3cos.iso    # Specify ISO path
#   ./system/scripts/run-qemu.sh --download            # Download latest release ISO
#
# Prerequisites:
#   brew install qemu   (macOS)
#   apt install qemu-system-x86  (Debian/Ubuntu)

set -e

# Handle --download flag
if [ "$1" = "--download" ]; then
    echo "Downloading latest W3C OS ISO from GitHub Releases..."
    ISO_URL=$(curl -s https://api.github.com/repos/wangnaihe/w3cos/releases/latest \
        | grep "browser_download_url.*iso" \
        | head -1 \
        | cut -d '"' -f 4)
    if [ -z "$ISO_URL" ]; then
        echo "No ISO found in latest release."
        exit 1
    fi
    mkdir -p output/images
    ISO="output/images/w3cos-latest.iso"
    curl -L -o "$ISO" "$ISO_URL"
    echo "Downloaded: $ISO"
else
    ISO="${1:-output/images/w3cos.iso}"
fi

if [ ! -f "$ISO" ]; then
    echo "ISO not found: $ISO"
    echo ""
    echo "Options:"
    echo "  1. Download pre-built:  $0 --download"
    echo "  2. Build from source:"
    echo "     a) cargo build --release -p w3cos-shell --target x86_64-unknown-linux-gnu"
    echo "     b) cd buildroot-2024.11"
    echo "     c) make BR2_EXTERNAL=\$(pwd)/../system/buildroot w3cos_x86_64_defconfig"
    echo "     d) make"
    echo "  3. Use GitHub Actions: push a tag (v0.1.0) to trigger build"
    exit 1
fi

echo "╔═══════════════════════════════════════╗"
echo "║     W3C OS — QEMU Virtual Machine     ║"
echo "╚═══════════════════════════════════════╝"
echo ""
echo "  ISO:  $ISO"
echo "  RAM:  2GB"
echo "  CPU:  2 cores"
echo "  SSH:  localhost:2222"
echo ""
echo "  Controls:"
echo "    Ctrl+Alt+F  — Toggle fullscreen"
echo "    Ctrl+A, X   — Exit QEMU"
echo ""

# Detect KVM availability
KVM_FLAG=""
if [ -e /dev/kvm ]; then
    KVM_FLAG="-enable-kvm"
    echo "  KVM:  enabled (hardware acceleration)"
else
    echo "  KVM:  not available (software emulation)"
fi
echo ""

qemu-system-x86_64 \
    -cdrom "$ISO" \
    -m 2G \
    -smp 2 \
    $KVM_FLAG \
    -vga virtio \
    -display sdl \
    -netdev user,id=net0,hostfwd=tcp::2222-:22 \
    -device virtio-net-pci,netdev=net0 \
    -serial mon:stdio \
    -name "W3C OS"
