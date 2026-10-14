#!/usr/bin/env bash
# Start a local OpenAI-compatible server for the shell's L2 planner.
# The shell does not spawn this. Leave W3COS_L2 unset and nothing dials out.
#
#   ./scripts/w3cos-inferd.sh llama-0.5b
#   ./scripts/w3cos-inferd.sh llama-1.5b
#   ./scripts/w3cos-inferd.sh ollama-0.5b
#
# Then, in another shell:
#   export W3COS_L2=1
#   export W3COS_L2_BASE_URL=http://127.0.0.1:8081/v1
#   export W3COS_L2_MODEL=qwen2.5-0.5b-instruct
#   export W3COS_L2_SERVER_PID=$(cat models/inferd.pid)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${W3COS_MODEL_DIR:-$ROOT/models}"
KIND="${1:-llama-1.5b}"
PORT="${W3COS_INFER_PORT:-8081}"
mkdir -p "$DEST"

start_llama() {
  local model="$1"
  local server="$DEST/llama-${W3COS_LLAMA_TAG:-b11514}/llama-server"
  if [[ ! -x "$server" || ! -f "$model" ]]; then
    echo "missing llama-server or $model — run scripts/fetch-l2-models.sh" >&2
    exit 1
  fi
  echo "llama-server $model on 127.0.0.1:$PORT"
  "$server" \
    -m "$model" \
    --host 127.0.0.1 \
    --port "$PORT" \
    -c 4096 \
    -t "${W3COS_INFER_THREADS:-4}" \
    --jinja \
    >"$DEST/inferd.log" 2>&1 &
  echo $! >"$DEST/inferd.pid"
  echo "pid $(cat "$DEST/inferd.pid"), log $DEST/inferd.log"
}

case "$KIND" in
  llama-0.5b)
    start_llama "$DEST/qwen2.5-0.5b-instruct-q4_k_m.gguf"
    ;;
  llama-1.5b)
    start_llama "$DEST/qwen2.5-1.5b-instruct-q4_k_m.gguf"
    ;;
  ollama-0.5b)
    if ! command -v ollama >/dev/null; then
      echo "ollama is not installed. https://ollama.com (MIT). The bench client is the same HTTP API." >&2
      exit 1
    fi
    ollama serve >"$DEST/ollama.log" 2>&1 &
    echo $! >"$DEST/inferd.pid"
    # Ollama's OpenAI port is 11434. Point the shell there.
    echo "ollama pid $(cat "$DEST/inferd.pid")"
    echo "export W3COS_L2_BASE_URL=http://127.0.0.1:11434/v1"
    echo "export W3COS_L2_MODEL=qwen2.5:0.5b"
    echo "ollama pull qwen2.5:0.5b"
    ;;
  *)
    echo "usage: $0 llama-0.5b|llama-1.5b|ollama-0.5b" >&2
    exit 2
    ;;
esac
