#!/usr/bin/env bash
# Quilt-style patch queue for Code-OSS (docs/roadmap/aot-port.md M1 item 9).
#
#   vscode-patches.sh fetch    shallow-fetch the pinned upstream commit into $VSCODE_DIR
#   vscode-patches.sh apply    reset $VSCODE_DIR to the pin and `git am` every patch in series
#   vscode-patches.sh refresh  rewrite patches/ + series from the commits on top of the pin
#   vscode-patches.sh lint     run `w3cos leak-lint` over $VSCODE_DIR/src (JSON + text + ranked markdown)
#   vscode-patches.sh check    fetch + apply + verify every patch carries a W3cos-Upstream trailer
#
# The pin is tools/vscode-baseline/CODE_OSS.lock.json (same commit as the M0 baseline).
# $VSCODE_DIR defaults to ports/vscode/.work/vscode (git-ignored scratch checkout).
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$here/../.." && pwd)"
patches="$here/patches"
series="$patches/series"
work="${VSCODE_DIR:-$here/.work/vscode}"
reports="${LEAK_LINT_REPORT_DIR:-$here/.work/reports}"
lock="$repo/tools/vscode-baseline/CODE_OSS.lock.json"
pin="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["commit"])' "$lock")"
upstream="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["repository"])' "$lock")"
git_id=(-c user.name="w3cos patch queue" -c user.email="patches@w3cos.invalid")

series_files() {
  grep -v '^\s*#' "$series" | sed '/^\s*$/d'
}

cmd_fetch() {
  mkdir -p "$work"
  if [ ! -d "$work/.git" ]; then
    git -C "$work" init -q
    git -C "$work" remote add origin "$upstream"
  fi
  git -C "$work" fetch -q --depth 1 origin "$pin"
  git -C "$work" checkout -q --detach "$pin"
  echo "code-oss $pin checked out in $work"
}

cmd_apply() {
  [ -d "$work/.git" ] || cmd_fetch
  git -C "$work" am --abort >/dev/null 2>&1 || true
  git -C "$work" checkout -q --detach "$pin"
  git -C "$work" reset -q --hard "$pin"
  local n=0
  while read -r patch; do
    git "${git_id[@]}" -C "$work" am -q --3way --committer-date-is-author-date "$patches/$patch" || {
      echo "error: $patch does not apply on $pin" >&2
      git -C "$work" am --abort >/dev/null 2>&1 || true
      exit 1
    }
    n=$((n + 1))
  done < <(series_files)
  echo "applied $n patch(es) on $pin"
}

cmd_refresh() {
  [ -d "$work/.git" ] || { echo "no checkout at $work (run fetch/apply first)" >&2; exit 1; }
  local tmp
  tmp="$(mktemp -d)"
  git -C "$work" format-patch -q --no-signature --zero-commit -N "$pin..HEAD" -o "$tmp"
  while read -r patch; do rm -f "$patches/$patch"; done < <(series_files)
  {
    echo "# Applied in order on top of tools/vscode-baseline/CODE_OSS.lock.json (\`vscode-patches.sh apply\`)."
    echo "# Each patch carries a 'W3cos-Upstream: yes|no' trailer (yes = should be sent to microsoft/vscode)."
    for file in "$tmp"/*.patch; do
      [ -e "$file" ] || continue
      name="$(basename "$file" | sed 's/^0*[0-9]*-//')"
      cp "$file" "$patches/$name"
      echo "$name"
    done
  } > "$series"
  rm -rf "$tmp"
  echo "refreshed $(series_files | wc -l | tr -d ' ') patch(es) from $pin..HEAD"
}

cmd_lint() {
  [ -d "$work/.git" ] || cmd_fetch
  mkdir -p "$reports"
  local bin="${W3COS_BIN:-}"
  if [ -z "$bin" ]; then
    cargo build -q --release -p w3cos-cli --manifest-path "$repo/Cargo.toml"
    bin="${CARGO_TARGET_DIR:-$repo/target}/release/w3cos"
  fi
  "$bin" leak-lint "$work/src" --exclude /test/ --fail-on never --json -o "$reports/code-oss-leak-lint.json"
  "$bin" leak-lint "$work/src" --exclude /test/ --fail-on never -o "$reports/code-oss-leak-lint.txt"
  python3 "$here/scripts/rank_findings.py" "$reports/code-oss-leak-lint.json" \
    --commit "$(git -C "$work" rev-parse HEAD)" --pin "$pin" > "$reports/code-oss-leak-lint.md"
  echo "reports in $reports"
}

cmd_check() {
  local missing=0
  while read -r patch; do
    if ! grep -Eq '^W3cos-Upstream: (yes|no)$' "$patches/$patch"; then
      echo "error: $patch has no 'W3cos-Upstream: yes|no' trailer" >&2
      missing=1
    fi
  done < <(series_files)
  [ "$missing" = 0 ] || exit 1
  cmd_fetch
  cmd_apply
}

case "${1:-}" in
  fetch | apply | refresh | lint | check) "cmd_$1" ;;
  *)
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
