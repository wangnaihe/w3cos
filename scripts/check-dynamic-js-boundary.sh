#!/usr/bin/env bash
set -euo pipefail

runtime_source="crates/w3cos-runtime/src/dynamic_script.rs"

ordinary_tree="$(cargo tree -p w3cos-runtime --no-default-features -e normal)"
for forbidden in w3cos-compiler w3cos-vm w3cos-ir swc_; do
    if grep -q "$forbidden" <<<"$ordinary_tree"; then
        echo "ordinary AOT runtime unexpectedly contains $forbidden" >&2
        exit 1
    fi
done

dynamic_tree="$(
    cargo tree \
        -p w3cos-runtime \
        --no-default-features \
        --features dynamic-js \
        -e normal
)"
for required in w3cos-compiler w3cos-vm w3cos-ir; do
    if ! grep -q "$required" <<<"$dynamic_tree"; then
        echo "dynamic browser runtime is missing $required" >&2
        exit 1
    fi
done

runtime_consumers="$(
    grep -R -l \
        -e 'w3cos_compiler' \
        -e 'w3cos_vm' \
        -e 'w3cos_ir' \
        crates/w3cos-runtime/src \
        | sort
)"
if [[ "$runtime_consumers" != "$runtime_source" ]]; then
    echo "dynamic compiler/IR/VM references escaped the single ScriptLoader boundary:" >&2
    echo "$runtime_consumers" >&2
    exit 1
fi

classic_lowerings="$(grep -c 'w3ir_lowering::lower_script' "$runtime_source")"
module_lowerings="$(grep -c 'w3ir_lowering::lower_module' "$runtime_source")"
vm_entries="$(grep -c 'Vm::new' "$runtime_source")"
if [[ "$classic_lowerings" -ne 1 || "$module_lowerings" -ne 1 || "$vm_entries" -ne 2 ]]; then
    echo "dynamic ScriptLoader must keep one classic lowering, one module lowering, and two W3VM construction sites" >&2
    exit 1
fi

if grep -Eq 'Command::new\([^)]*"rustc"|process::Command[^;]*rustc' "$runtime_source"; then
    echo "dynamic ScriptLoader must never invoke rustc" >&2
    exit 1
fi

for required_route in \
    'ScriptExecutionRoute::PrecompiledAot' \
    'url.scheme() == "file"' \
    'resolve_precompiled_aot_specifier' \
    'contains_native' \
    'never fall back to W3VM'
do
    if ! grep -q "$required_route" "$runtime_source"; then
        echo "dynamic ScriptLoader is missing protocol route guard: $required_route" >&2
        exit 1
    fi
done

echo "network/inline scripts use SWC -> W3IR -> W3VM; file scripts require registered native AOT"
