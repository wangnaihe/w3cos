#!/bin/bash
# Quick-start W3C OS in QEMU virtual machine.
#
# Usage:
#   ./system/scripts/run-qemu.sh                    # Boot from ISO
#   ./system/scripts/run-qemu.sh path/to/w3cos.iso  # Specify ISO path
#
# Prerequisites:
#   brew install qemu   (macOS)
#   apt install qemu-system-x86  (Debian/Ubuntu)

set -e

ISO="${1:-output/images/w3cos.iso}"

if [ ! -f "$ISO" ]; then
    echo "ISO not found: $ISO"
    echo ""
    echo "Build it first:"
    echo "  1. Download Buildroot: https://buildroot.org/download.html"
    echo "  2. cd buildroot-2024.11"
    echo "  3. make BR2_EXTERNAL=$(pwd)/../system/buildroot w3cos_x86_64_defconfig"
    echo "  4. make"
    echo "  5. The ISO will be in output/images/"
    echo ""
    echo "Or use the pre-built ISO from GitHub Releases."
    exit 1
fi

echo "Starting W3C OS in QEMU..."
echo "  ISO: $ISO"
echo "  RAM: 2GB"
echo "  Press Ctrl+A, X to exit"
echo ""

qemu-system-x86_64 \
    -cdrom "$ISO" \
    -m 2G \
    -smp 2 \
    -enable-kvm 2>/dev/null || true \
    -vga virtio \
    -display sdl \
    -netdev user,id=net0,hostfwd=tcp::2222-:22 \
    -device virtio-net-pci,netdev=net0 \
    -serial mon:stdio
