#!/usr/bin/env bash
# Rebuild CJK-Subset.ttf from a system CJK font + demo string inventory.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AINATIVE="$(cd "$ROOT/../.." && pwd)"
OUT="$ROOT/crates/w3cos-runtime/assets/CJK-Subset.ttf"
CHARS_FILE="$(mktemp)"
TEXT_FILE="$(mktemp)"
trap 'rm -f "$CHARS_FILE" "$TEXT_FILE"' EXIT

python3 <<PY > "$TEXT_FILE"
import re
from pathlib import Path

roots = [
    Path("$AINATIVE/demo/native/logidesk-focus.tsx"),
    Path("$AINATIVE/demo/native/logidesk-focus.css"),
]
chars = set(chr(i) for i in range(32, 127))
chars.update("·→▼✕✦×—…，。、；：？！（）《》「」【】")
for path in roots:
    if path.exists():
        text = path.read_text(encoding="utf-8")
        for m in re.finditer(r">([^<{]+)<", text):
            chars.update(m.group(1).strip())
        chars.update(re.findall(r"[\u4e00-\u9fff\u3000-\u303f\uff00-\uffef]", text))
print("".join(sorted(chars, key=ord)))
PY

# Prefer full CJK-capable sources on macOS / Linux.
SOURCE=""
for candidate in \
  "/System/Library/Fonts/PingFang.ttc" \
  "/System/Library/Fonts/Supplemental/Arial Unicode.ttf" \
  "/Library/Fonts/Arial Unicode.ttf" \
  "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc" \
  "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"; do
  if [[ -f "$candidate" ]]; then
    SOURCE="$candidate"
    break
  fi
done

if [[ -z "$SOURCE" ]]; then
  echo "error: no CJK source font found" >&2
  exit 1
fi

echo "==> Source font: $SOURCE"
echo "==> Unique chars: $(wc -m < "$TEXT_FILE" | tr -d ' ')"

if ! command -v pyftsubset >/dev/null 2>&1; then
  echo "==> Installing fonttools..."
  python3 -m pip install -q fonttools
fi

ARGS=(--output-file="$OUT" --text-file="$TEXT_FILE" --layout-features="*" --glyph-names --symbol-cmap --legacy-cmap)
if [[ "$SOURCE" == *.ttc ]]; then
  ARGS+=(--font-number=0)
fi

pyftsubset "$SOURCE" "${ARGS[@]}"
echo "✅ Wrote $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes)"
