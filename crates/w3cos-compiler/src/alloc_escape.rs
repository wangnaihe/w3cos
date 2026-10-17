//! Intra-procedural allocation escape analysis on W3IR (AOT M1-4a, part 1).
//!
//! Ordinary AOT object and array literals live in the page arena: `Copy`
//! `u32` handles with no reference count, freed only on page reset. This pass
//! finds literals that provably never leave the function's own frame. The
//! AOT backend allocates those as reference-counted host values
//! (`Value::local_object` / `Value::local_array`), so they are freed at their
//! last reference, at the latest when the function returns.
//!
//! # Model
//!
//! A unification-based (Steensgaard-style) points-to analysis over registers
//! and local bindings. Every equivalence class has at most one *content*
//! class: the values stored inside objects of that class. `Move`,
//! `LoadBinding`, `StoreBinding` and `InitializeBinding` unify their operands.
//! Literal property values, `SetProperty` / `DefineField` /
//! `AppendArrayElement` values and `GetProperty` results unify with the
//! container's content class.
//!
//! # Escape rules (conservative)
//!
//! A class escapes when any member register is used anywhere except:
//! - `DefineField` with a constant key (how literal members are lowered);
//!   it extends the class shape and never runs a prototype setter;
//! - `GetProperty` / `SetProperty` *object* operand with a key the class
//!   shape owns (objects: a constant key present in every literal of the
//!   class or defined on the class; arrays: a proven number, an index
//!   constant or `"length"`), so no getter, setter or proxy trap on a
//!   prototype can receive the object as `this`;
//! - `AppendArrayElement` array operand;
//! - `Branch` condition, `typeof`, and `===` / `!==` operands (no user code).
//!
//! Everything else escapes: returns, throws, any call / construct argument,
//! callee or receiver, `await` / `yield` operands, arithmetic and other
//! coercions (`valueOf` / `toString` may run user code), computed keys,
//! delete, spread / rest / copy, private fields, class creation, and dynamic
//! import. Bindings escape when captured by a closure, captured from an
//! outer function, or when they are parameters, `this`, `arguments` or the
//! rest parameter. In async functions and generators, registers live across
//! a suspension point escape. A function that references `eval` (as a
//! captured binding, a local name or a string constant) escapes everything. If a class escapes, its content escapes too. A class that can
//! reach itself through its content (e.g. `o.self = o`) also escapes, because
//! a reference-counted cycle would never be freed.
//!
//! Soundness note: a misclassified literal still behaves correctly (host
//! `Rc` values are valid anywhere); classification only decides *when* the
//! memory is reclaimed.

use std::collections::{HashMap, HashSet};

use w3cos_ir::{BinaryOperator, Constant, Function, Instruction, UnaryOperator};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AllocEscape {
    /// Destination registers of `CreateObject` / `CreateArray` whose
    /// allocations never escape the frame.
    pub local_registers: HashSet<u32>,
    /// Allocation sites (`CreateObject` + `CreateArray`) in the function.
    pub sites: usize,
    /// Allocation sites proven local.
    pub local_sites: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Object,
    Array,
}

struct Graph {
    parent: Vec<usize>,
    content: Vec<Option<usize>>,
}

impl Graph {
    fn new(nodes: usize) -> Self {
        Self {
            parent: (0..nodes).collect(),
            content: vec![None; nodes],
        }
    }

    fn add(&mut self) -> usize {
        let id = self.parent.len();
        self.parent.push(id);
        self.content.push(None);
        id
    }

    fn find(&mut self, mut node: usize) -> usize {
        while self.parent[node] != node {
            self.parent[node] = self.parent[self.parent[node]];
            node = self.parent[node];
        }
        node
    }

    fn unify(&mut self, a: usize, b: usize) {
        let mut work = vec![(a, b)];
        while let Some((a, b)) = work.pop() {
            let (ra, rb) = (self.find(a), self.find(b));
            if ra == rb {
                continue;
            }
            self.parent[rb] = ra;
            match (self.content[ra], self.content[rb]) {
                (Some(ca), Some(cb)) => work.push((ca, cb)),
                (None, Some(cb)) => self.content[ra] = Some(cb),
                _ => {}
            }
        }
    }

    fn content_of(&mut self, node: usize) -> usize {
        let root = self.find(node);
        if let Some(content) = self.content[root] {
            return content;
        }
        let content = self.add();
        self.content[root] = Some(content);
        content
    }
}

/// Registers defined exactly once, by a `LoadConstant`.
fn single_constants(function: &Function) -> HashMap<u32, Constant> {
    let mut defs: HashMap<u32, usize> = HashMap::new();
    let mut constants = HashMap::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            if let Some(dst) = crate::w3ir_aot::written_register_id(instruction) {
                *defs.entry(dst).or_default() += 1;
            }
            if let Instruction::LoadConstant { dst, value } = instruction {
                constants.insert(dst.0, value.clone());
            }
        }
    }
    constants.retain(|register, _| defs.get(register) == Some(&1));
    constants
}

fn mentions_eval(function: &Function, is_eval_binding: &dyn Fn(u32) -> bool) -> bool {
    function
        .captures
        .iter()
        .any(|binding| is_eval_binding(binding.0))
        || function
            .bindings
            .iter()
            .any(|binding| binding.name == "eval")
        || function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::LoadConstant { value: Constant::String(text), .. } if text == "eval"
            )
        })
        })
}

/// Classify every object/array literal in `function`. `is_number` reports
/// registers proven to always hold numbers (array index keys);
/// `is_eval_binding` reports outer bindings named `eval`.
pub fn analyze(
    function: &Function,
    is_number: &dyn Fn(u32) -> bool,
    is_eval_binding: &dyn Fn(u32) -> bool,
) -> AllocEscape {
    let registers = function.registers as usize;
    let binding_node: HashMap<u32, usize> = function
        .bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| (binding.id.0, registers + index))
        .chain(
            function
                .captures
                .iter()
                .enumerate()
                .map(|(index, binding)| (binding.0, registers + function.bindings.len() + index)),
        )
        .collect();
    let mut graph = Graph::new(registers + function.bindings.len() + function.captures.len());
    let mut escaping: Vec<usize> = Vec::new();
    let constants = single_constants(function);
    let mut sites: Vec<(u32, Kind, Option<HashSet<String>>)> = Vec::new();

    // Bindings whose cells outlive or are shared beyond this frame.
    let binding = |graph: &mut Graph, id: u32| -> usize {
        match binding_node.get(&id) {
            Some(node) => *node,
            None => graph.add(),
        }
    };
    let mut shared_bindings: Vec<u32> = function.captures.iter().map(|b| b.0).collect();
    shared_bindings.extend(function.parameters.iter().map(|b| b.0));
    shared_bindings.extend(function.rest_parameter.map(|b| b.0));
    shared_bindings.extend(function.arguments_binding.map(|b| b.0));
    shared_bindings.extend(function.this_binding.map(|b| b.0));
    for block in &function.blocks {
        for instruction in &block.instructions {
            if let Instruction::CreateClosure { captures, .. } = instruction {
                shared_bindings.extend(captures.iter().map(|b| b.0));
            }
        }
    }
    for id in shared_bindings {
        let node = binding(&mut graph, id);
        escaping.push(node);
    }

    if function.is_async || function.is_generator {
        for point in &function.suspension_points {
            escaping.extend(point.live_registers.iter().map(|r| r.0 as usize));
        }
        for point in &function.generator_suspension_points {
            escaping.extend(point.live_registers.iter().map(|r| r.0 as usize));
        }
    }

    let string_key = |register: u32| match constants.get(&register) {
        Some(Constant::String(key)) => Some(key.clone()),
        Some(Constant::Number(number)) => Some(number_key(*number)),
        _ => None,
    };
    // Accesses are checked against the class shape once classes are final.
    // `AppendArrayElement` records `numeric = true` with no key.
    let mut accesses: Vec<(u32, Option<String>, bool)> = Vec::new();
    let mut defined: Vec<(u32, String)> = Vec::new();

    for block in &function.blocks {
        for instruction in &block.instructions {
            match instruction {
                Instruction::Move { dst, src } => {
                    graph.unify(dst.0 as usize, src.0 as usize);
                }
                Instruction::LoadBinding { dst, binding: id } => {
                    let node = binding(&mut graph, id.0);
                    graph.unify(dst.0 as usize, node);
                }
                Instruction::StoreBinding { binding: id, value }
                | Instruction::InitializeBinding { binding: id, value } => {
                    let node = binding(&mut graph, id.0);
                    graph.unify(node, value.0 as usize);
                }
                Instruction::CreateObject { dst, properties } => {
                    let mut keys = Some(HashSet::new());
                    let content = graph.content_of(dst.0 as usize);
                    for (key, value) in properties {
                        match string_key(key.0) {
                            Some(key) if key != "__proto__" => {
                                if let Some(keys) = keys.as_mut() {
                                    keys.insert(key);
                                }
                            }
                            _ => {
                                keys = None;
                                escaping.push(key.0 as usize);
                            }
                        }
                        graph.unify(content, value.0 as usize);
                    }
                    if keys.is_none() {
                        escaping.push(dst.0 as usize);
                    }
                    sites.push((dst.0, Kind::Object, keys));
                }
                Instruction::CreateArray { dst, elements } => {
                    let content = graph.content_of(dst.0 as usize);
                    for element in elements {
                        graph.unify(content, element.0 as usize);
                    }
                    sites.push((dst.0, Kind::Array, None));
                }
                Instruction::AppendArrayElement { array, value } => {
                    let content = graph.content_of(array.0 as usize);
                    graph.unify(content, value.0 as usize);
                    accesses.push((array.0, None, true));
                }
                Instruction::GetProperty { dst, object, key } => {
                    let content = graph.content_of(object.0 as usize);
                    graph.unify(content, dst.0 as usize);
                    accesses.push((object.0, string_key(key.0), is_number(key.0)));
                }
                Instruction::DefineField { object, key, value } => {
                    // Literal members lower to `CreateObject {}` + `DefineField`.
                    // [[Define]] never runs a prototype setter, so a constant key
                    // extends the class shape; a computed key escapes.
                    let content = graph.content_of(object.0 as usize);
                    graph.unify(content, value.0 as usize);
                    match string_key(key.0) {
                        Some(key) if key != "__proto__" => defined.push((object.0, key)),
                        _ => {
                            escaping.push(object.0 as usize);
                            escaping.push(key.0 as usize);
                        }
                    }
                }
                Instruction::SetProperty {
                    object, key, value, ..
                } => {
                    let content = graph.content_of(object.0 as usize);
                    graph.unify(content, value.0 as usize);
                    accesses.push((object.0, string_key(key.0), is_number(key.0)));
                }
                Instruction::Branch { .. }
                | Instruction::LoadConstant { .. }
                | Instruction::RefreshBinding { .. }
                | Instruction::Jump { .. } => {}
                Instruction::Unary {
                    operator: UnaryOperator::TypeOf,
                    ..
                } => {}
                Instruction::Binary {
                    operator: BinaryOperator::StrictEqual | BinaryOperator::StrictNotEqual,
                    ..
                } => {}
                other => {
                    escaping.extend(
                        crate::w3ir_aot::read_registers(other)
                            .into_iter()
                            .map(|register| register as usize),
                    );
                }
            }
        }
    }

    // Class kinds and shapes.
    let mut class_kind: HashMap<usize, Option<Kind>> = HashMap::new();
    let mut class_keys: HashMap<usize, Option<HashSet<String>>> = HashMap::new();
    for (register, kind, keys) in &sites {
        let root = graph.find(*register as usize);
        class_kind
            .entry(root)
            .and_modify(|existing| {
                if *existing != Some(*kind) {
                    *existing = None;
                }
            })
            .or_insert(Some(*kind));
        let merged = class_keys.entry(root).or_insert_with(|| keys.clone());
        *merged = match (merged.take(), keys) {
            (Some(left), Some(right)) => Some(left.intersection(right).cloned().collect()),
            _ => None,
        };
    }
    for (object, key) in defined {
        let root = graph.find(object as usize);
        match class_keys.get_mut(&root) {
            Some(Some(keys)) => {
                keys.insert(key);
            }
            Some(None) => {}
            // Defining onto something that is not one of our literals.
            None => escaping.push(object as usize),
        }
    }
    for (object, key, numeric) in accesses {
        let root = graph.find(object as usize);
        let Some(kind) = class_kind.get(&root) else {
            continue;
        };
        let allowed = match kind {
            Some(Kind::Object) => key.as_ref().is_some_and(|key| {
                class_keys
                    .get(&root)
                    .and_then(Option::as_ref)
                    .is_some_and(|keys| keys.contains(key))
            }),
            Some(Kind::Array) => {
                numeric
                    || key.as_deref() == Some("length")
                    || key.as_deref().is_some_and(is_index_key)
            }
            None => false,
        };
        if !allowed {
            escaping.push(object as usize);
        }
    }

    // Propagate escape to contents.
    let mut escaped: HashSet<usize> = HashSet::new();
    let mut work: Vec<usize> = if mentions_eval(function, is_eval_binding) {
        (0..graph.parent.len()).collect()
    } else {
        escaping
    };
    while let Some(node) = work.pop() {
        let root = graph.find(node);
        if escaped.insert(root)
            && let Some(content) = graph.content[root]
        {
            work.push(content);
        }
    }

    let mut result = AllocEscape {
        sites: sites.len(),
        ..AllocEscape::default()
    };
    for (register, _, _) in &sites {
        let root = graph.find(*register as usize);
        if escaped.contains(&root) || reaches_itself(&mut graph, root) {
            continue;
        }
        result.local_registers.insert(*register);
    }
    result.local_sites = sites
        .iter()
        .filter(|(register, _, _)| result.local_registers.contains(register))
        .count();
    result
}

fn reaches_itself(graph: &mut Graph, root: usize) -> bool {
    let mut seen = HashSet::new();
    let mut node = graph.content[root];
    while let Some(next) = node {
        let next = graph.find(next);
        if next == root {
            return true;
        }
        if !seen.insert(next) {
            return false;
        }
        node = graph.content[next];
    }
    false
}

fn is_index_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn number_key(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        format!("{number}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::w3ir_aot::generate_sync_function_from_module;
    use crate::w3ir_lowering::lower_script;

    fn classify(source: &str, name: &str) -> (AllocEscape, String) {
        let module = lower_script(source, "app:///alloc-escape.js").unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("missing function {name}"));
        let escape = analyze(function, &|_| false, &|id| {
            module.functions.iter().any(|function| {
                function
                    .bindings
                    .iter()
                    .any(|binding| binding.id.0 == id && binding.name == "eval")
            })
        });
        let generated = if function.is_async || function.is_generator {
            String::new()
        } else {
            generate_sync_function_from_module(&module, function, &format!("{name}_aot")).unwrap()
        };
        (escape, generated)
    }

    fn assert_local(source: &str, name: &str, local: usize, total: usize) -> String {
        let (escape, generated) = classify(source, name);
        assert_eq!(
            (escape.local_sites, escape.sites),
            (local, total),
            "{name}: local/total sites\n{generated}"
        );
        generated
    }

    #[test]
    fn loop_temporary_with_known_shape_is_local() {
        let generated = assert_local(
            r#"
            function work(n) {
              let sum = 0;
              for (let i = 0; i < n; i++) {
                const p = { x: i, y: i + 1 };
                sum += p.x + p.y;
              }
              return sum;
            }
            work;
            "#,
            "work",
            1,
            1,
        );
        assert!(generated.contains("w3cos_core::Value::local_object("));
    }

    #[test]
    fn local_array_indexed_by_constants_is_local() {
        assert_local(
            r#"
            function pair(a, b) {
              const t = [a, b];
              t[0] = t[1];
              return t[0] + t.length;
            }
            pair;
            "#,
            "pair",
            1,
            1,
        );
    }

    #[test]
    fn nested_literal_stays_local_with_container() {
        assert_local(
            r#"
            function nested(n) {
              const o = { inner: { v: n } };
              return o.inner.v;
            }
            nested;
            "#,
            "nested",
            2,
            2,
        );
    }

    #[test]
    fn escaping_uses_keep_arena_allocation() {
        let cases = [
            ("function f() { return { a: 1 }; } f;", "f"),
            (
                "const kept = []; function f() { kept.push({ a: 1 }); return 0; } f;",
                "f",
            ),
            (
                "function f(g) { const o = { a: 1 }; g(o); return 0; } f;",
                "f",
            ),
            ("function f() { const o = { a: 1 }; throw o; } f;", "f"),
            (
                "function f() { const o = { a: 1 }; return () => o.a; } f;",
                "f",
            ),
            (
                "function f(x) { const o = { a: 1 }; x.o = o; return 0; } f;",
                "f",
            ),
            ("function f() { const o = { a: 1 }; return o + 1; } f;", "f"),
            ("function f(k) { const o = { a: 1 }; return o[k]; } f;", "f"),
            (
                "function f() { const o = { a: 1 }; return o.toString; } f;",
                "f",
            ),
            (
                "function f() { const o = { a: 1 }; o.self = o; return 0; } f;",
                "f",
            ),
            (
                "function f() { const o = { a: 1 }; return eval('o'); } f;",
                "f",
            ),
            ("function f(p) { p = { a: 1 }; return p.a; } f;", "f"),
            (
                "function f() { const inner = { v: 1 }; const o = { a: inner }; return o.a; } f;",
                "f",
            ),
        ];
        for (source, name) in cases {
            let (escape, generated) = classify(source, name);
            let expected_local =
                usize::from(source.contains("o.a; } f;") && source.contains("inner"));
            assert_eq!(
                escape.local_sites, expected_local,
                "{source} should keep its escaping literal in the arena\n{generated}"
            );
        }
    }

    #[test]
    fn async_and_generator_suspension_keeps_live_literals_in_arena() {
        let (escape, _) = classify(
            "async function f(p) { const o = { a: 1 }; await p; return o.a; } f;",
            "f",
        );
        assert_eq!(escape.local_sites, 0);
        let (escape, _) = classify(
            "function* g() { const o = { a: 1 }; yield 1; return o.a; } g;",
            "g",
        );
        assert_eq!(escape.local_sites, 0);
    }
}
