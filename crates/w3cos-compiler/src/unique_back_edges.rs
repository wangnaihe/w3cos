//! Local unique-back-edge detection for AOT WeakRef rewriting.
//!
//! This is not an escape analyzer. It matches the W3IR shapes that uniquely
//! identify a back-edge from a closure onto an object it captures:
//!
//! 1. Self-install: `CreateClosure` capturing `B`, then `SetProperty` /
//!    `DefineField` of that closure onto a register loaded from `B`, with a
//!    static key (`obj.fn = () => obj.x`, `this.fn = () => this.x`).
//! 2. Promise reaction: `CallMethod` `"then"` / `"catch"` / `"finally"` on
//!    binding `B` whose argument is a closure capturing `B`.
//! 3. Iterator-on-collection: a closure capturing `B` is stored on a fresh
//!    object (object literal), and that object is stored onto `B` with a
//!    static key (`collection.iter = { next: () => collection.pop() }`).
//!
//! Warn-only (no rewrite) when which edge to weaken is ambiguous:
//! - mutual properties (`a.b = b; b.a = a`)
//! - computed / dynamic keys (`obj[k] = ...`)
//! - cross-file cycles (closure captures an import and is stored on a
//!   different local binding)
//!
//! Dynamic assignments are never auto-weakened: dropping that ownership edge
//! would make the stored value collectable while still reachable from the
//! computed key.

use std::collections::{HashMap, HashSet};
use w3cos_ir::{BindingId, BindingKind, Constant, Function, FunctionId, Instruction, Register};

const PROMISE_REACTION_KEYS: &[&str] = &["then", "catch", "finally"];

/// Nested function + captured binding that AOT should box as `WeakRef`.
#[derive(Clone, Debug, Default)]
pub struct UniqueBackEdgePlan {
    pub weak_captures: HashSet<(FunctionId, BindingId)>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
enum RegOrigin {
    Binding(BindingId),
    Closure {
        function: FunctionId,
        captures: Vec<BindingId>,
    },
    Fresh(u32),
}

pub fn analyze_unique_back_edges(
    function: &Function,
    specifier: Option<&str>,
) -> UniqueBackEdgePlan {
    let mut weak_captures = HashSet::new();
    let mut warnings = Vec::new();
    let mut property_edges: HashSet<(u32, u32)> = HashSet::new();
    let mut saw_dynamic_assignment = false;
    let mut binding_wrapper_closures: HashMap<BindingId, Vec<(FunctionId, Vec<BindingId>)>> =
        HashMap::new();
    let mut closure_hosts: HashMap<FunctionId, HashSet<BindingId>> = HashMap::new();
    let import_bindings = function
        .bindings
        .iter()
        .filter(|binding| binding.kind == BindingKind::Import)
        .map(|binding| binding.id)
        .collect::<HashSet<_>>();

    for block in &function.blocks {
        let mut origins: HashMap<u32, RegOrigin> = HashMap::new();
        let mut fresh_closures: HashMap<u32, Vec<(FunctionId, Vec<BindingId>)>> = HashMap::new();
        for (index, instruction) in block.instructions.iter().enumerate() {
            let proven = proven_string_keys(&block.instructions, index);
            match instruction {
                Instruction::LoadBinding { dst, binding } => {
                    origins.insert(dst.0, RegOrigin::Binding(*binding));
                }
                Instruction::Move { dst, src } => {
                    if let Some(origin) = origins.get(&src.0).cloned() {
                        origins.insert(dst.0, origin);
                    } else {
                        origins.remove(&dst.0);
                    }
                }
                Instruction::CreateClosure {
                    dst,
                    function: nested,
                    captures,
                } => {
                    origins.insert(
                        dst.0,
                        RegOrigin::Closure {
                            function: *nested,
                            captures: captures.clone(),
                        },
                    );
                }
                Instruction::CreateObject { dst, properties } => {
                    origins.insert(dst.0, RegOrigin::Fresh(dst.0));
                    let mut stored = Vec::new();
                    for (_key, value) in properties {
                        push_closure_origin(&origins, value, &mut stored);
                    }
                    if !stored.is_empty() {
                        fresh_closures.insert(dst.0, stored);
                    }
                }
                Instruction::CreateArray { dst, .. } => {
                    origins.insert(dst.0, RegOrigin::Fresh(dst.0));
                }
                Instruction::InitializeBinding { binding, value }
                | Instruction::StoreBinding { binding, value } => {
                    if let Some(RegOrigin::Fresh(fresh)) = origins.get(&value.0) {
                        if let Some(closures) = fresh_closures.get(fresh) {
                            binding_wrapper_closures.insert(*binding, closures.clone());
                        }
                    }
                    if let Some(RegOrigin::Closure {
                        function: nested,
                        captures,
                    }) = origins.get(&value.0)
                    {
                        binding_wrapper_closures
                            .insert(*binding, vec![(*nested, captures.clone())]);
                    }
                }
                Instruction::SetProperty { object, key, value }
                | Instruction::DefineField { object, key, value } => {
                    note_fresh_closure(&origins, &mut fresh_closures, *object, *value);
                    if proven.get(&key.0).is_none() {
                        saw_dynamic_assignment = true;
                    } else {
                        record_store(
                            &origins,
                            &fresh_closures,
                            &binding_wrapper_closures,
                            &mut property_edges,
                            &mut weak_captures,
                            &mut closure_hosts,
                            *object,
                            *value,
                        );
                    }
                }
                Instruction::DefinePrivate { object, value, .. } => {
                    note_fresh_closure(&origins, &mut fresh_closures, *object, *value);
                    record_store(
                        &origins,
                        &fresh_closures,
                        &binding_wrapper_closures,
                        &mut property_edges,
                        &mut weak_captures,
                        &mut closure_hosts,
                        *object,
                        *value,
                    );
                }
                Instruction::CallMethod {
                    dst,
                    object,
                    key,
                    arguments,
                } => {
                    if let Some(method) = proven.get(&key.0) {
                        if PROMISE_REACTION_KEYS.contains(&method.as_str()) {
                            if let Some(RegOrigin::Binding(host)) = origins.get(&object.0).cloned()
                            {
                                for argument in arguments {
                                    if let Some(RegOrigin::Closure {
                                        function: nested,
                                        captures,
                                    }) = origins.get(&argument.0)
                                    {
                                        if captures.contains(&host) {
                                            weak_captures.insert((*nested, host));
                                            closure_hosts.entry(*nested).or_default().insert(host);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    origins.remove(&dst.0);
                }
                other => {
                    if let Some(dst) = written_register(other) {
                        origins.remove(&dst.0);
                    }
                }
            }
        }
    }

    let where_at = specifier
        .map(|specifier| format!(" in {specifier}"))
        .unwrap_or_default();
    if property_edges
        .iter()
        .any(|(left, right)| property_edges.contains(&(*right, *left)))
    {
        warnings.push(format!(
            "ambiguous unique WeakRef back-edge: mutual properties{where_at}"
        ));
    }
    if saw_dynamic_assignment {
        warnings.push(format!(
            "ambiguous unique WeakRef back-edge: dynamic property assignment{where_at}"
        ));
    }

    for block in &function.blocks {
        for instruction in &block.instructions {
            let Instruction::CreateClosure {
                function: nested,
                captures,
                ..
            } = instruction
            else {
                continue;
            };
            let hosts = closure_hosts.get(nested);
            for capture in captures {
                if !import_bindings.contains(capture) {
                    continue;
                }
                if weak_captures.contains(&(*nested, *capture)) {
                    continue;
                }
                let stored_elsewhere =
                    hosts.is_some_and(|hosts| hosts.iter().any(|host| host != capture));
                if stored_elsewhere {
                    warnings.push(format!(
                        "ambiguous unique WeakRef back-edge: cross-file capture cycle{where_at}"
                    ));
                    break;
                }
            }
        }
    }

    warnings.sort();
    warnings.dedup();
    UniqueBackEdgePlan {
        weak_captures,
        warnings,
    }
}

fn record_store(
    origins: &HashMap<u32, RegOrigin>,
    fresh_closures: &HashMap<u32, Vec<(FunctionId, Vec<BindingId>)>>,
    binding_wrapper_closures: &HashMap<BindingId, Vec<(FunctionId, Vec<BindingId>)>>,
    property_edges: &mut HashSet<(u32, u32)>,
    weak_captures: &mut HashSet<(FunctionId, BindingId)>,
    closure_hosts: &mut HashMap<FunctionId, HashSet<BindingId>>,
    object: Register,
    value: Register,
) {
    let Some(object_origin) = origins.get(&object.0) else {
        return;
    };
    match (object_origin, origins.get(&value.0)) {
        (
            RegOrigin::Binding(host),
            Some(RegOrigin::Closure {
                function: nested,
                captures,
            }),
        ) => {
            closure_hosts.entry(*nested).or_default().insert(*host);
            if captures.contains(host) {
                weak_captures.insert((*nested, *host));
            }
        }
        (RegOrigin::Binding(host), Some(RegOrigin::Fresh(fresh))) => {
            if let Some(closures) = fresh_closures.get(fresh) {
                for (nested, captures) in closures {
                    closure_hosts.entry(*nested).or_default().insert(*host);
                    if captures.contains(host) {
                        weak_captures.insert((*nested, *host));
                    }
                }
            }
        }
        (RegOrigin::Binding(host), Some(RegOrigin::Binding(held))) => {
            if host != held {
                property_edges.insert((host.0, held.0));
            }
            if let Some(closures) = binding_wrapper_closures.get(held) {
                for (nested, captures) in closures {
                    closure_hosts.entry(*nested).or_default().insert(*host);
                    if captures.contains(host) {
                        weak_captures.insert((*nested, *host));
                    }
                }
            }
        }
        _ => {}
    }
}

fn note_fresh_closure(
    origins: &HashMap<u32, RegOrigin>,
    fresh_closures: &mut HashMap<u32, Vec<(FunctionId, Vec<BindingId>)>>,
    object: Register,
    value: Register,
) {
    if let (
        Some(RegOrigin::Fresh(fresh)),
        Some(RegOrigin::Closure {
            function: nested,
            captures,
        }),
    ) = (origins.get(&object.0), origins.get(&value.0))
    {
        fresh_closures
            .entry(*fresh)
            .or_default()
            .push((*nested, captures.clone()));
    }
}

fn push_closure_origin(
    origins: &HashMap<u32, RegOrigin>,
    value: &Register,
    stored: &mut Vec<(FunctionId, Vec<BindingId>)>,
) {
    if let Some(RegOrigin::Closure {
        function: nested,
        captures,
    }) = origins.get(&value.0)
    {
        stored.push((*nested, captures.clone()));
    }
}

fn proven_string_keys(instructions: &[Instruction], index: usize) -> HashMap<u32, String> {
    let mut known = HashMap::new();
    for instruction in &instructions[..index] {
        match instruction {
            Instruction::LoadConstant {
                dst,
                value: Constant::String(value),
            } => {
                known.insert(dst.0, value.clone());
            }
            other => {
                if let Some(dst) = written_register(other) {
                    known.remove(&dst.0);
                }
            }
        }
    }
    known
}

fn written_register(instruction: &Instruction) -> Option<Register> {
    match instruction {
        Instruction::LoadConstant { dst, .. }
        | Instruction::Move { dst, .. }
        | Instruction::LoadBinding { dst, .. }
        | Instruction::Add { dst, .. }
        | Instruction::Binary { dst, .. }
        | Instruction::Unary { dst, .. }
        | Instruction::GetProperty { dst, .. }
        | Instruction::DeleteProperty { dst, .. }
        | Instruction::GetPrivate { dst, .. }
        | Instruction::HasPrivate { dst, .. }
        | Instruction::CreateArray { dst, .. }
        | Instruction::ArrayRest { dst, .. }
        | Instruction::ObjectRest { dst, .. }
        | Instruction::CreateObject { dst, .. }
        | Instruction::CreateClosure { dst, .. }
        | Instruction::CreateClass { dst, .. }
        | Instruction::Call { dst, .. }
        | Instruction::CallWithArguments { dst, .. }
        | Instruction::CallMethod { dst, .. }
        | Instruction::CallMethodWithArguments { dst, .. }
        | Instruction::Construct { dst, .. }
        | Instruction::ConstructWithArguments { dst, .. }
        | Instruction::DynamicImport { dst, .. }
        | Instruction::ImportMeta { dst }
        | Instruction::Await { dst, .. }
        | Instruction::Yield { dst, .. }
        | Instruction::YieldDelegate { dst, .. } => Some(*dst),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::w3ir_aot::generate_sync_function_from_module;
    use crate::w3ir_lowering::{lower_module, lower_script};

    fn named_function<'a>(module: &'a w3cos_ir::Module, name: &str) -> &'a Function {
        module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("missing function {name}"))
    }

    fn analyze_named(source: &str, name: &str, specifier: &str) -> (UniqueBackEdgePlan, String) {
        let module = lower_script(source, specifier).unwrap();
        let function = named_function(&module, name);
        let plan = analyze_unique_back_edges(function, Some(specifier));
        let generated =
            generate_sync_function_from_module(&module, function, &format!("{name}_aot")).unwrap();
        (plan, generated)
    }

    fn is_rewritten(generated: &str) -> bool {
        generated.contains("w3cos_core::weak::weak_ref_class")
            && generated.contains("call_method(\"deref\"")
    }

    #[test]
    fn unique_self_capturing_closure_is_rewritten_to_weakref() {
        let (plan, generated) = analyze_named(
            r#"
                function install() {
                    const obj = { x: 1 };
                    obj.fn = () => obj.x;
                    return obj;
                }
                install;
            "#,
            "install",
            "app:///unique-self-install.js",
        );
        assert!(
            !plan.weak_captures.is_empty(),
            "unique self-install must be selected for WeakRef rewrite: {plan:?}"
        );
        assert!(
            is_rewritten(&generated),
            "AOT must box the unique self-capture as WeakRef and deref on use: {generated}"
        );
        assert!(
            plan.warnings.is_empty(),
            "unique self-install must not warn: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn promise_reaction_unique_capture_is_rewritten_to_weakref() {
        let (plan, generated) = analyze_named(
            r#"
                function track(job) {
                    job.then(() => job.done);
                    job.catch(() => job.failed);
                    job.finally(() => job.closed);
                    return job;
                }
                track;
            "#,
            "track",
            "app:///unique-promise-reaction.js",
        );
        assert!(
            !plan.weak_captures.is_empty(),
            "unique Promise reaction capture must be selected for WeakRef rewrite: {plan:?}"
        );
        assert!(
            is_rewritten(&generated),
            "AOT must box the unique Promise reaction capture as WeakRef: {generated}"
        );
    }

    #[test]
    fn iterator_on_collection_unique_capture_is_rewritten_to_weakref() {
        // IR shape this matches: CreateClosure capturing the collection,
        // DefineField of that closure onto a fresh object literal, then
        // SetProperty of that wrapper onto the collection with a static key.
        // If lowering ever stops emitting that local store chain, skip rewrite
        // rather than inventing an escape analyzer.
        let (plan, generated) = analyze_named(
            r#"
                function attach(collection) {
                    const iterator = { next: () => collection.pop() };
                    collection.iter = iterator;
                    return collection;
                }
                attach;
            "#,
            "attach",
            "app:///unique-iterator-on-collection.js",
        );
        assert!(
            !plan.weak_captures.is_empty(),
            "iterator-on-collection unique capture must be selected for WeakRef rewrite: {plan:?}"
        );
        assert!(
            is_rewritten(&generated),
            "AOT must box the iterator-on-collection capture as WeakRef: {generated}"
        );
    }

    #[test]
    fn mutual_properties_warn_and_are_not_rewritten() {
        let (plan, generated) = analyze_named(
            r#"
                function cycle(a, b) {
                    a.b = b;
                    b.a = a;
                    return a;
                }
                cycle;
            "#,
            "cycle",
            "app:///mutual-properties.js",
        );
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("mutual properties")),
            "mutual properties must warn: {:?}",
            plan.warnings
        );
        assert!(
            plan.weak_captures.is_empty(),
            "mutual properties must not rewrite: {plan:?}"
        );
        assert!(
            !is_rewritten(&generated),
            "mutual properties must not emit WeakRef captures: {generated}"
        );
    }

    #[test]
    fn dynamic_assignment_is_not_rewritten() {
        let (plan, generated) = analyze_named(
            r#"
                function assign(obj, key) {
                    obj[key] = () => obj.x;
                    return obj;
                }
                assign;
            "#,
            "assign",
            "app:///dynamic-assignment.js",
        );
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("dynamic property assignment")),
            "dynamic assignment must warn: {:?}",
            plan.warnings
        );
        assert!(
            plan.weak_captures.is_empty(),
            "dynamic assignment must not rewrite a unique back-edge: {plan:?}"
        );
        assert!(
            !is_rewritten(&generated),
            "dynamic assignment must not emit WeakRef captures: {generated}"
        );
    }

    #[test]
    fn cross_file_import_capture_stored_on_local_warns_without_rewrite() {
        let module = lower_module(
            r#"
                import { remote } from "./remote.js";
                export const local = {};
                local.fn = () => remote.x;
            "#,
            "app:///cross-file-cycle.js",
        )
        .unwrap();
        let entry = module
            .functions
            .iter()
            .find(|function| function.id == module.entry)
            .unwrap();
        let plan = analyze_unique_back_edges(entry, Some("app:///cross-file-cycle.js"));
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("cross-file")),
            "capturing an import and storing the closure on a local object must warn: {:?}",
            plan.warnings
        );
        assert!(
            plan.weak_captures.is_empty(),
            "cross-file cycles are warn-only: {plan:?}"
        );
    }
}
