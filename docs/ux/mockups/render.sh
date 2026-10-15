#!/bin/sh
# Render docs/ux/mockups/*.html to docs/ux/screenshots/*.png.
# Usage: docs/ux/mockups/render.sh
set -eu
cd "$(dirname "$0")"
python3 build.py
out=../screenshots
mkdir -p "$out"
shot() {
  name=$1
  w=$2
  h=$3
  timeout 20s google-chrome --headless=new --no-sandbox --disable-gpu \
    --disable-dev-shm-usage --hide-scrollbars --force-device-scale-factor=1 \
    --window-size="${w},${h}" \
    --screenshot="$out/${name}.png" \
    "file://$(pwd)/${name}.html" >/dev/null 2>&1 || true
}
shot desktop-home 1440 900
shot desktop-confirm 1440 900
shot desktop-permission 1440 900
shot desktop-receipts 1440 900
shot desktop-lock 1440 900
shot desktop-launcher 1440 900
shot desktop-notifications 1440 900
shot desktop-windows 1440 900
shot state-empty 1440 900
shot state-loading 1440 900
shot state-error 1440 900
shot phone-home 390 844
shot phone-confirm 390 844
shot phone-switcher 390 844
shot phone-notifications 390 844
shot tablet-home 1194 834
shot handoff-pair 1440 900
shot handoff-desktop 1440 900
shot handoff-phone 390 844
