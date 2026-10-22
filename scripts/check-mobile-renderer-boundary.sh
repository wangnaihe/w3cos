#!/usr/bin/env bash
# A mobile artifact links exactly one primary renderer.
# Skia must not pull softbuffer/vello/wgpu. The gpu closure must not pull Skia
# or softbuffer. resvg always links tiny-skia, so the crate name is not banned.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

skia_features="skia,image-png,image-jpeg,image-webp"
gpu_features="gpu,web-graphics-advanced,image-png,image-jpeg,image-webp"

tree() {
    cargo tree -p w3cos-runtime --no-default-features --features "$1" -e normal
}

contains_package() {
    local features="$1"
    local package="$2"
    cargo tree -p w3cos-runtime --no-default-features --features "$features" -e normal -i "$package" >/dev/null 2>&1
}

skia_tree="$(tree "$skia_features")"
if ! grep -q 'skia-safe' <<<"$skia_tree"; then
    echo "mobile skia closure is missing skia-safe" >&2
    exit 1
fi
for forbidden in softbuffer vello wgpu ravif rav1e; do
    if contains_package "$skia_features" "$forbidden"; then
        echo "mobile skia closure unexpectedly contains $forbidden" >&2
        exit 1
    fi
done
# resvg always links the gif crate. Gate the image crate's own format features.
image_features="$(cargo tree -p w3cos-runtime --no-default-features --features "$skia_features" -e features -i image)"
for forbidden in gif bmp ico avif; do
    if grep -q "image feature \"$forbidden\"" <<<"$image_features"; then
        echo "mobile skia closure unexpectedly enables image/$forbidden" >&2
        exit 1
    fi
done
for required in png jpeg webp; do
    if ! grep -q "image feature \"$required\"" <<<"$image_features"; then
        echo "mobile skia closure is missing image/$required" >&2
        exit 1
    fi
done

gpu_tree="$(tree "$gpu_features")"
if ! grep -Eq 'vello|wgpu' <<<"$gpu_tree"; then
    echo "mobile gpu closure is missing vello/wgpu" >&2
    exit 1
fi
for forbidden in skia-safe softbuffer ravif rav1e; do
    if contains_package "$gpu_features" "$forbidden"; then
        echo "mobile gpu closure unexpectedly contains $forbidden" >&2
        exit 1
    fi
done

python3 - <<'PY'
from pathlib import Path
import re

failures = []
for path in Path("crates").rglob("*.rs"):
    text = path.read_text()
    if 'include_bytes!("../assets/CJK-Subset.ttf")' not in text:
        continue
    if path.name == "system_fonts.rs":
        if not re.search(
            r'#\[cfg\(any\(test, feature = "embed-cjk-font"\)\)\]\s*fn cjk_subset_bytes',
            text,
        ):
            failures.append(f"{path}: cjk_subset_bytes is not gated on test or embed-cjk-font")
        helper = re.search(
            r'fn cjk_subset_bytes\(\) -> &\'static \[u8\] \{\s*include_bytes!\("\.\./assets/CJK-Subset\.ttf"\)\s*\}',
            text,
        )
        if helper is None:
            failures.append(f"{path}: expected a single gated include_bytes helper")
        continue
    head = re.split(r'(?m)^#\[cfg\(test\)\]', text, maxsplit=1)[0]
    if "CJK-Subset.ttf" in head and not re.search(
        r'#\[cfg\(test\)\][\s\S]{0,240}CJK-Subset\.ttf', head
    ):
        failures.append(f"{path}: CJK subset is included outside cfg(test)")

web = Path("crates/w3cos-compiler/src/web_codegen.rs").read_text()
copy_at = web.find("std::fs::copy")
window = web[max(0, copy_at - 500):copy_at]
if "embed_cjk" not in window:
    failures.append("web codegen copies CJK-Subset.ttf without the embed flag")

if failures:
    print("\n".join(failures))
    raise SystemExit(1)
PY

echo "mobile renderer boundary ok"
