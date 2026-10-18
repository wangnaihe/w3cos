#!/usr/bin/env bash
# test262 conformance through the AOT pipeline (M1 item 14).
#
#   scripts/test262.sh fetch            # checkout the pinned commit into $TEST262_DIR
#   scripts/test262.sh subset [args]    # CI gate: VS Code-priority subset vs expected failures
#   scripts/test262.sh full [args]      # full built-ins + language + annexB run, writes reports
#   scripts/test262.sh update-expected  # regenerate tests/test262/expected-failures.txt
#
# TEST262_DIR defaults to target/test262; reports go to $TEST262_OUT (target/test262-report).
set -euo pipefail
cd "$(dirname "$0")/.."
LOCK=$(tr -d '[:space:]' < tests/test262/test262.lock)
DIR=${TEST262_DIR:-target/test262}
OUT=${TEST262_OUT:-target/test262-report}

fetch() {
  if [ -d "$DIR/.git" ] && [ "$(git -C "$DIR" rev-parse HEAD)" = "$LOCK" ]; then
    return
  fi
  rm -rf "$DIR"
  mkdir -p "$DIR"
  git -C "$DIR" init -q
  git -C "$DIR" remote add origin https://github.com/tc39/test262.git
  git -C "$DIR" fetch -q --depth 1 origin "$LOCK"
  git -C "$DIR" checkout -q FETCH_HEAD
}

runner() {
  # Batch crates build with --offline against the workspace lockfile.
  cargo fetch -q
  cargo run --release -q -p w3cos-compiler --example test262 -- --test262 "$DIR" "$@"
}

cmd=${1:-subset}
shift || true
case "$cmd" in
  fetch) fetch ;;
  subset)
    fetch
    mkdir -p "$OUT"
    runner --subset tests/test262/subset.txt --expected tests/test262/expected-failures.txt \
      --report "$OUT/subset.json" --summary "$OUT/subset.md" "$@"
    ;;
  update-expected)
    fetch
    runner --subset tests/test262/subset.txt --expected tests/test262/expected-failures.txt --update-expected "$@"
    ;;
  full)
    fetch
    mkdir -p "$OUT"
    runner --report "$OUT/full.json" --summary "$OUT/full.md" "$@"
    ;;
  *) echo "usage: $0 fetch|subset|full|update-expected" >&2; exit 2 ;;
esac
