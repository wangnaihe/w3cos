#!/usr/bin/env bash
# Download llama.cpp's llama-server and the Qwen2.5 GGUF weights used by the
# L2 bench. Nothing here belongs in git.
#
# Licenses:
#   llama.cpp  MIT  https://github.com/ggml-org/llama.cpp
#   Qwen2.5    Apache-2.0  https://huggingface.co/Qwen
#
# Usage:
#   ./scripts/fetch-l2-models.sh
#   ./scripts/w3cos-inferd.sh llama-0.5b
#   ./scripts/w3cos-inferd.sh llama-1.5b
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${W3COS_MODEL_DIR:-$ROOT/models}"
LLAMA_TAG="${W3COS_LLAMA_TAG:-b11514}"
mkdir -p "$DEST"

fetch() {
  local url="$1"
  local out="$2"
  if [[ -f "$out" ]]; then
    echo "have $out"
    return
  fi
  echo "get $url"
  curl -fL --retry 3 --retry-delay 2 -o "$out.partial" "$url"
  mv "$out.partial" "$out"
}

fetch \
  "https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_TAG}/llama-${LLAMA_TAG}-bin-ubuntu-x64.tar.gz" \
  "$DEST/llama-${LLAMA_TAG}-bin-ubuntu-x64.tar.gz"

if [[ ! -x "$DEST/llama-${LLAMA_TAG}/llama-server" ]]; then
  tar -xzf "$DEST/llama-${LLAMA_TAG}-bin-ubuntu-x64.tar.gz" -C "$DEST"
fi

fetch \
  "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf" \
  "$DEST/qwen2.5-0.5b-instruct-q4_k_m.gguf"

fetch \
  "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf" \
  "$DEST/qwen2.5-1.5b-instruct-q4_k_m.gguf"

echo
echo "sha256:"
sha256sum \
  "$DEST/llama-${LLAMA_TAG}/llama-server" \
  "$DEST/qwen2.5-0.5b-instruct-q4_k_m.gguf" \
  "$DEST/qwen2.5-1.5b-instruct-q4_k_m.gguf"
echo "weights stay in $DEST and are gitignored"
