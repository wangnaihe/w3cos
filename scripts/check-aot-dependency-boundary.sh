#!/usr/bin/env bash
set -euo pipefail

dependency_tree="$(cargo tree -p w3cos-runtime --no-default-features -e normal)"
for forbidden in w3cos-compiler w3cos-vm w3cos-ir swc_; do
    if grep -q "$forbidden" <<<"$dependency_tree"; then
        echo "ordinary AOT runtime unexpectedly contains $forbidden" >&2
        exit 1
    fi
done

compiler_codegen="crates/w3cos-compiler/src/esm_codegen.rs"
compiler_codegen_production="$(sed '/^#\[cfg(test)\]/,$d' "$compiler_codegen")"
if grep -q \
    -e 'InitChunk::Unsupported' \
    -e 'W3IR module-init chunks.*unavailable' \
    -e 'ctx\.lower_stmt(statement)' \
    -e 'fn find_function(' \
    -e 'fn lower_dynamic_params(' \
    -e 'fn lower_dynamic_pattern(' \
    -e 'let mut field_inits' \
    -e 'let mut static_inits' \
    -e 'instance_field_key_defs' \
    -e 'ctx\.lower_stmts(&block\.body\.stmts)' \
    -e 'base_ctx()\.lower_expr' \
    -e 'W3IR AOT synchronous function emission failed' \
    -e 'W3IR AOT async emission failed' \
    -e 'W3IR AOT generator emission failed' \
    -e 'W3IR AOT class member emission failed' \
    -e 'W3IR AOT class constructor emission failed' \
    <<<"$compiler_codegen_production"; then
    echo "native W3IR generation regained a direct-AST or runtime-stub fallback" >&2
    exit 1
fi

if ! grep -q 'try_generate_with_bodies_and_css' crates/w3cos-compiler/src/lib.rs; then
    echo "the production compiler must propagate native W3IR codegen failures" >&2
    exit 1
fi
if ! grep -q 'downcast_ref::<esm_codegen::EsmCodegenError>' crates/w3cos-compiler/src/lib.rs; then
    echo "the production compiler must not downgrade W3IR codegen errors to the legacy transpiler" >&2
    exit 1
fi
if ! grep -q 'W3IR native function emission' "$compiler_codegen"; then
    echo "native function W3IR failures lack compile-time diagnostics" >&2
    exit 1
fi
if ! grep -q 'W3IR native class emission' "$compiler_codegen"; then
    echo "native class W3IR failures lack compile-time diagnostics" >&2
    exit 1
fi

echo "ordinary AOT excludes compiler/W3VM/W3IR/SWC linkage; module init, functions and class callables have one W3IR semantic path"
