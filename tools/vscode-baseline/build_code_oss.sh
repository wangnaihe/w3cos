#!/usr/bin/env bash
# Build the pinned official Code-OSS (Electron) tree. This is side A of the
# M0 baseline. It is not a w3cos compile. PR CI does not run this script:
# set W3COS_BUILD_CODE_OSS=1 to run it on a machine with Node, a C++ toolchain,
# and enough disk and RAM for the upstream gulp task.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LOCK="$ROOT/tools/vscode-baseline/CODE_OSS.lock.json"
DEST="$ROOT/target/code-oss"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest)
      DEST="$2"
      shift 2
      ;;
    --lock)
      LOCK="$2"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ "${GITHUB_ACTIONS:-}" == "true" && "${W3COS_BUILD_CODE_OSS:-}" != "1" ]]; then
  echo "Refusing to compile Code-OSS in GitHub Actions without W3COS_BUILD_CODE_OSS=1." >&2
  echo "PR CI writes the dependency inventory and the port-analyze report only." >&2
  exit 2
fi

COMMIT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["commit"])' "$LOCK")"
REPO="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["repository"])' "$LOCK")"

mkdir -p "$(dirname "$DEST")"
if [[ ! -d "$DEST/.git" ]]; then
  git init "$DEST"
  git -C "$DEST" remote add origin "$REPO"
fi
git -C "$DEST" fetch --depth 1 origin "$COMMIT"
git -C "$DEST" checkout --detach FETCH_HEAD

python3 "$ROOT/tools/vscode-baseline/inventory.py" \
  --root "$DEST" \
  --lock "$LOCK" \
  --check \
  --output "$DEST/w3cos-dependency-inventory.json"

command -v node >/dev/null
command -v npm >/dev/null
NODE_ACTUAL="$(node -p 'process.version.slice(1)')"
NODE_EXPECTED="$(tr -d '[:space:]' < "$DEST/.nvmrc")"
if [[ "$NODE_ACTUAL" != "$NODE_EXPECTED" ]]; then
  echo "Node is $NODE_ACTUAL; the pin wants $NODE_EXPECTED (.nvmrc)." >&2
  exit 1
fi

(cd "$DEST" && npm ci)
(cd "$DEST" && npm run gulp -- vscode-linux-x64)

PARENT="$(cd "$(dirname "$DEST")" && pwd)"
PACKAGE_DIR="$PARENT/VSCode-linux-x64"
BINARY=""
for candidate in "$PACKAGE_DIR/bin/code-oss" "$PACKAGE_DIR/code-oss"; do
  if [[ -x "$candidate" ]]; then
    BINARY="$candidate"
    break
  fi
done
if [[ -z "$BINARY" ]]; then
  echo "vscode-linux-x64 finished but code-oss was not found under $PACKAGE_DIR" >&2
  exit 1
fi

python3 - "$LOCK" "$BINARY" "$DEST/w3cos-dependency-inventory.json" "$PARENT/code-oss-baseline-manifest.json" <<'PY'
import hashlib, json, sys
lock_path, binary, inventory_path, manifest_path = sys.argv[1:]
lock = json.load(open(lock_path))
inventory = json.load(open(inventory_path))
data = open(binary, "rb").read()
manifest = {
    "schema": "w3cos-code-oss-baseline-manifest-v1",
    "commit": lock["commit"],
    "binary": binary,
    "binary_sha256": hashlib.sha256(data).hexdigest(),
    "binary_bytes": len(data),
    "inventory": inventory,
}
with open(manifest_path, "w") as handle:
    json.dump(manifest, handle, indent=2)
    handle.write("\n")
print(manifest_path)
PY
