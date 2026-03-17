#!/bin/bash
set -e

echo "=== W3C OS: Setting up development environment ==="

# Install system dependencies for rendering
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    libfontconfig1-dev \
    fonts-dejavu-core \
    qemu-system-x86

# Build the project
echo "=== Building W3C OS ==="
cargo build --release

echo ""
echo "=== W3C OS dev environment ready ==="
echo ""
echo "Quick start:"
echo "  cargo build --release"
echo "  ./target/release/w3cos build examples/showcase/app.ts -o showcase --release"
echo "  ./showcase"
echo ""
