# W3C OS — Installation Guide

## Three Ways to Run W3C OS

### 1. Fastest: Docker (compile apps only, no GUI)

```bash
docker build -t w3cos .
docker run w3cos build /apps/hello/app.ts -o /tmp/hello --release
```

### 2. Development: DevContainer / Codespaces

Open this repo in GitHub Codespaces or any DevContainer-compatible IDE.
Everything (Rust, QEMU, fonts) is installed automatically.

```bash
# In Codespaces / VS Code DevContainer:
cargo build --release
./target/release/w3cos build examples/showcase/app.ts -o showcase --release
./showcase
```

### 3. Full OS: Bootable ISO (QEMU or real hardware)

#### Quick Start with Pre-built ISO

```bash
# Download from GitHub Releases
wget https://github.com/anthropic-ai/w3cos/releases/download/v0.1/w3cos-0.1-x86_64.iso

# Run in QEMU
qemu-system-x86_64 -cdrom w3cos-0.1-x86_64.iso -m 2G -vga virtio

# Or flash to USB (CAUTION: this erases the USB drive)
sudo dd if=w3cos-0.1-x86_64.iso of=/dev/sdX bs=4M status=progress
# Boot your computer from the USB drive
```

#### Build ISO from Source

Prerequisites:
- Linux host (or Docker on macOS)
- 10GB free disk space
- Rust toolchain
- Build tools: `sudo apt install build-essential ncurses-dev wget python3`

```bash
# One command builds everything:
./system/scripts/build-iso.sh

# Output: w3cos.iso (~50-100MB)

# Test in QEMU:
./system/scripts/run-qemu.sh w3cos.iso
```

#### What the ISO Contains

```
w3cos.iso (~50-100MB)
├── Linux kernel 6.x (bzImage)        — hardware drivers, process/memory mgmt
├── Firmware blobs                     — WiFi, GPU, Bluetooth support
├── BusyBox                            — minimal shell (for maintenance/SSH)
├── W3C OS Shell (w3cos-shell)         — THE system interface (replaces desktop)
├── SSH server (dropbear)              — remote access for AI agents
├── Default apps (showcase, hello)     — pre-installed W3C OS applications
└── Boot: BIOS/UEFI → kernel → init → w3cos-shell (fullscreen)
```

#### Boot Sequence

```
Power On
  → BIOS/UEFI
  → GRUB/syslinux bootloader
  → Linux kernel loads
  → Hardware drivers initialize
  → Init system starts
  → W3C OS Shell launches (fullscreen)
  → User sees W3C OS interface — no login screen, no desktop
  → SSH available on port 22 for AI agent access
```

## System Requirements

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| CPU | x86_64 (any) | Intel Core i3+ / AMD Ryzen 3+ |
| RAM | 512 MB | 2 GB |
| Storage | 128 MB (live boot) | 1 GB (installed) |
| Display | 800x600 | 1920x1080 |
| GPU | Framebuffer (any) | Intel/AMD with mesa support |
| Network | Optional | Ethernet or supported WiFi |

> **Note**: W3C OS currently supports **x86_64 only**. ARM64 cross-compilation and Android container support (via Waydroid) are planned for Phase 3 — see [ROADMAP.md](/ROADMAP.md). Mobile phone flashing is not yet supported.

## Remote Access (for AI Agents)

The ISO includes an SSH server. After booting:

```bash
# From another machine (or AI agent):
ssh root@<w3cos-ip>

# Password: none (key-based auth recommended for production)

# Build and run apps remotely:
w3cos build /apps/hello/app.ts -o /tmp/hello --release
```

## Supported Hardware

W3C OS inherits Linux kernel hardware support:
- **CPU**: Intel, AMD (x86_64). ARM64 support planned.
- **GPU**: Intel (i915), AMD (amdgpu), NVIDIA (nouveau), VirtIO (QEMU)
- **WiFi**: Intel (iwlwifi), Realtek, Ralink, Atheros
- **Bluetooth**: Most USB and integrated adapters
- **Storage**: SATA, NVMe, USB, SD card
- **Input**: USB keyboard/mouse, touchpad, touchscreen

Drivers come from the Linux kernel + linux-firmware package — same as Debian/Ubuntu.
