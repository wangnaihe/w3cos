#!/usr/bin/env bash
# Build the ordinary-AOT and dynamic-browser linkage probes, attribute the
# unstripped symbol tables, and compare stripped sizes with the checked-in
# same-target baseline.
#
# Regression gate: an increase of more than 3% or more than 200 KiB versus the
# row for this rustc host triple. Other architectures are not compared.
#
# Update the row for this machine only, after review:
#   scripts/audit-linkage-sizes.sh --update-baseline --baseline-note "why the size changed"
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

update=0
note=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --update-baseline)
            update=1
            shift
            ;;
        --baseline-note)
            note="${2:-}"
            shift 2
            ;;
        *)
            echo "unknown argument: $1" >&2
            echo "usage: scripts/audit-linkage-sizes.sh [--update-baseline --baseline-note <text>]" >&2
            exit 1
            ;;
    esac
done

if [[ "$update" -eq 1 && -z "${note// }" ]]; then
    echo "--baseline-note is required with --update-baseline" >&2
    exit 1
fi

host="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -z "$host" ]]; then
    echo "rustc -vV did not report a host triple" >&2
    exit 1
fi

# Keep symbols for attribution, then strip a copy the same way the size gate
# measures the release artifact. The release-size profile itself still strips.
export CARGO_PROFILE_RELEASE_SIZE_STRIP=none

cargo build -p w3cos-size-audit --profile release-size
cargo build -p w3cos-size-probes --bin w3cos-aot-size-probe --profile release-size
cargo build -p w3cos-size-probes --features browser --bin w3cos-browser-size-probe --profile release-size

# Leave the cargo outputs unstripped so a later run can attribute without
# relinking. The size gate measures a stripped copy.
mkdir -p target/size-stripped
cp -a target/release-size/w3cos-aot-size-probe target/size-stripped/w3cos-aot-size-probe
cp -a target/release-size/w3cos-browser-size-probe target/size-stripped/w3cos-browser-size-probe
strip --strip-all target/size-stripped/w3cos-aot-size-probe
strip --strip-all target/size-stripped/w3cos-browser-size-probe

test "$(target/size-stripped/w3cos-aot-size-probe)" = "ordinary-aot"
test "$(target/size-stripped/w3cos-browser-size-probe)" = "dynamic-w3vm"

baseline=(
    --target "$host"
    --baseline-file tools/size-audit/baselines/linkage-probes.json
    --embedded-root crates/w3cos-runtime/assets
    --require-symbols
)
if [[ "$update" -eq 1 ]]; then
    baseline+=(--update-baseline --baseline-note "$note")
else
    baseline+=(--require-baseline)
fi

aot_bytes="$(stat -c%s target/size-stripped/w3cos-aot-size-probe)"
audit=(target/release-size/w3cos-size-audit)

"${audit[@]}" \
    --profile aot \
    --probe aot \
    --probe-description "Ordinary DOM/jsdom AOT linkage. No default runtime features and no dynamic-js." \
    --component executable=target/size-stripped/w3cos-aot-size-probe \
    --symbols target/release-size/w3cos-aot-size-probe \
    --markdown target/size-audit-aot.md \
    --output target/size-audit-aot.json \
    "${baseline[@]}"

"${audit[@]}" \
    --profile browser \
    --probe browser \
    --feature dynamic-js \
    --probe-description "Dynamic browser linkage: ordinary AOT plus SWC, W3IR, W3VM, the network script loader, and WOFF/WOFF2." \
    --component executable=target/size-stripped/w3cos-browser-size-probe \
    --symbols target/release-size/w3cos-browser-size-probe \
    --baseline-bytes "$aot_bytes" \
    --max-regression-percent 50 \
    --markdown target/size-audit-browser.md \
    --output target/size-audit-browser.json \
    "${baseline[@]}"
