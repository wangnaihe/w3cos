//! Native Rust state-machine emission from validated W3IR.
//!
//! This backend is deliberately compile-time only: generated applications
//! depend on `w3cos-core`, not `w3cos-ir` or `w3cos-vm`. W3VM and this emitter
//! therefore consume the same suspension/control-flow records without placing
//! a second JavaScript semantic implementation in ordinary AOT artifacts.

use crate::unique_back_edges::{UniqueBackEdgePlan, analyze_unique_back_edges};
use anyhow::{Result, anyhow, bail};
use std::collections::{HashMap, HashSet};
use w3cos_ir::{
    BinaryOperator, BindingKind, BlockId, Constant, Function, FunctionId, Instruction, Module,
    Register, UnaryOperator,
};

#[derive(Clone, Copy)]
enum EmissionMode {
    Generator,
    AsyncGenerator,
    Async,
    Sync,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegClass {
    Bottom,
    Number,
    Bool,
    String,
    Unknown,
}

fn join_class(left: RegClass, right: RegClass) -> RegClass {
    match (left, right) {
        (RegClass::Bottom, other) | (other, RegClass::Bottom) => other,
        (left, right) if left == right => left,
        _ => RegClass::Unknown,
    }
}

/// Per-function register lattice. A register is `Number` only when every
/// definition is Number. Locals that are captured by a closure stay boxed.
/// Locals that are only ever numbers, and are assigned before every load,
/// live in `num_bindings` as bare `f64`s.
///
/// Number and Bool registers occupy compact banks (`num_regs` / `bool_regs`)
/// and never keep a `Value` slot. Every other IR register maps into the
/// dense `registers` Value bank.
struct EscapePlan {
    number: HashSet<u32>,
    bools: HashSet<u32>,
    num_of: Vec<Option<u32>>,
    bool_of: Vec<Option<u32>>,
    val_of: Vec<Option<u32>>,
    n_num: u32,
    n_bool: u32,
    n_val: u32,
    /// Local binding id → slot in `num_bindings`.
    num_binding_of: HashMap<u32, u32>,
    n_num_bindings: u32,
    /// Binding id → number that is stored exactly once as a literal.
    constants: HashMap<u32, f64>,
    /// `CreateObject` / `CreateArray` destinations proven not to escape the
    /// frame ([`crate::alloc_escape`]): allocated as refcounted host values
    /// so they are freed at their last reference instead of page reset.
    local_allocs: HashSet<u32>,
}

thread_local! {
    static NUMERIC_CONSTANTS: std::cell::RefCell<HashMap<u32, f64>> =
        std::cell::RefCell::new(HashMap::new());
    static MODULE_FUNCTIONS: std::cell::RefCell<Vec<Function>> = std::cell::RefCell::new(Vec::new());
    /// Binding ids named `eval` across `MODULE_FUNCTIONS`, computed lazily
    /// (keyed by the module function count) for allocation escape analysis.
    static EVAL_BINDINGS: std::cell::RefCell<Option<(usize, HashSet<u32>)>> =
        const { std::cell::RefCell::new(None) };
    static SHARED_NUMBERS: std::cell::RefCell<HashSet<u32>> = std::cell::RefCell::new(HashSet::new());
    /// Outermost `generate_function_tree` owns the thread-locals above.
    /// An empty numeric-constant map is a valid install, so emptiness cannot
    /// mark that owner.
    static AOT_MODULE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct NumericConstantGuard(bool);

impl Drop for NumericConstantGuard {
    fn drop(&mut self) {
        AOT_MODULE_DEPTH.with(|depth| {
            depth.set(depth.get().saturating_sub(1));
        });
        if self.0 {
            NUMERIC_CONSTANTS.with(|constants| constants.borrow_mut().clear());
            MODULE_FUNCTIONS.with(|functions| functions.borrow_mut().clear());
            EVAL_BINDINGS.with(|cache| *cache.borrow_mut() = None);
        }
    }
}

struct SharedNumberGuard;

impl Drop for SharedNumberGuard {
    fn drop(&mut self) {
        SHARED_NUMBERS.with(|numbers| numbers.borrow_mut().clear());
    }
}

fn is_shared_number(id: u32) -> bool {
    SHARED_NUMBERS.with(|numbers| numbers.borrow().contains(&id))
}

/// Parent-side closure that is only `captured = captured + k`.
fn number_bump_capture(nested: FunctionId) -> Option<(u32, f64)> {
    MODULE_FUNCTIONS.with(|functions| {
        let functions = functions.borrow();
        let function = functions.iter().find(|function| function.id == nested)?;
        let (binding, addend) = match_number_increment(function)?;
        is_shared_number(binding).then_some((binding, addend))
    })
}

fn binding_is_used(function: &Function, id: u32) -> bool {
    if captured_binding_ids(function).contains(&id) {
        return true;
    }
    function.blocks.iter().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| match instruction {
                Instruction::LoadBinding { binding, .. }
                | Instruction::StoreBinding { binding, .. }
                | Instruction::InitializeBinding { binding, .. }
                | Instruction::RefreshBinding { binding } => binding.0 == id,
                _ => false,
            })
    })
}

fn binding_stores_are_numbers(function: &Function, binding: u32) -> bool {
    let mut assume = HashSet::new();
    assume.insert(binding);
    let classes = register_classes(function, &assume, &HashMap::new());
    let number_regs = classes
        .iter()
        .enumerate()
        .filter(|(_, class)| **class == RegClass::Number)
        .map(|(index, _)| index as u32)
        .collect::<HashSet<_>>();
    for block in &function.blocks {
        for instruction in &block.instructions {
            let source = match instruction {
                Instruction::InitializeBinding { binding: id, value }
                | Instruction::StoreBinding { binding: id, value }
                    if id.0 == binding =>
                {
                    Some(value.0)
                }
                _ => None,
            };
            let Some(source) = source else {
                continue;
            };
            if !number_regs.contains(&source) {
                return false;
            }
        }
    }
    true
}

fn function_touches_binding(function: &Function, binding: u32) -> bool {
    function.captures.iter().any(|capture| capture.0 == binding)
        || binding_is_used(function, binding)
}

/// Captured `var` bindings whose every store, including inside nested
/// functions, is a number. They live in one shared `Cell<Option<f64>>`.
fn shared_numeric_ids(function: &Function) -> HashSet<u32> {
    let captured = captured_binding_ids(function);
    let others = MODULE_FUNCTIONS.with(|functions| functions.borrow().clone());
    let mut shared = HashSet::new();
    for binding in &function.bindings {
        let id = binding.id.0;
        if !captured.contains(&id)
            || !binding.mutable
            || binding.kind != BindingKind::Var
            || !binding_stores_are_numbers(function, id)
        {
            continue;
        }
        let nested_ok = others
            .iter()
            .filter(|other| other.id != function.id)
            .all(|other| {
                if !function_touches_binding(other, id) {
                    return true;
                }
                binding_stores_are_numbers(other, id)
            });
        if nested_ok {
            shared.insert(id);
        }
    }
    shared
}

/// `binding = binding + addend` with no other effect, returning undefined.
fn match_number_increment(function: &Function) -> Option<(u32, f64)> {
    if function.is_async
        || function.is_generator
        || !function.exception_regions.is_empty()
        || !function.parameters.is_empty()
        || function.rest_parameter.is_some()
        || function.arguments_binding.is_some()
        || function.blocks.len() != 1
    {
        return None;
    }
    if function
        .this_binding
        .is_some_and(|binding| binding_is_used(function, binding.0))
    {
        return None;
    }
    let instructions = &function.blocks[0].instructions;
    let mut loaded: Option<(u32, u32)> = None;
    let mut constant: Option<(u32, f64)> = None;
    let mut added: Option<u32> = None;
    let mut stored = false;
    let mut saw_return = false;
    for instruction in instructions {
        match instruction {
            Instruction::LoadBinding { dst, binding } => {
                if loaded.is_some() {
                    return None;
                }
                loaded = Some((dst.0, binding.0));
            }
            Instruction::LoadConstant {
                dst,
                value: Constant::Number(value),
            } => {
                if constant.is_some() {
                    return None;
                }
                constant = Some((dst.0, *value));
            }
            Instruction::LoadConstant {
                value: Constant::Undefined,
                ..
            } => {}
            Instruction::Add { dst, lhs, rhs } => {
                let (loaded_reg, _) = loaded?;
                let (constant_reg, _) = constant?;
                let paired = (*lhs == Register(loaded_reg) && *rhs == Register(constant_reg))
                    || (*lhs == Register(constant_reg) && *rhs == Register(loaded_reg));
                if !paired {
                    return None;
                }
                added = Some(dst.0);
            }
            Instruction::StoreBinding { binding, value } => {
                let (_, loaded_binding) = loaded?;
                if binding.0 != loaded_binding || added != Some(value.0) {
                    return None;
                }
                stored = true;
            }
            Instruction::Return { .. } => {
                if !stored {
                    return None;
                }
                saw_return = true;
            }
            _ => return None,
        }
    }
    if !stored || !saw_return {
        return None;
    }
    let (_, binding) = loaded?;
    let (_, addend) = constant?;
    if !function.captures.iter().any(|capture| capture.0 == binding) {
        return None;
    }
    if function
        .captures
        .iter()
        .any(|capture| capture.0 != binding && binding_is_used(function, capture.0))
    {
        return None;
    }
    Some((binding, addend))
}

fn emit_number_bump_prefix(function: &Function) -> String {
    let Some((binding, addend)) = match_number_increment(function) else {
        return String::new();
    };
    format!(
        "    if let Some(w3cos_core::AotCapture::Number(cell)) = __captures.get(&{binding}) {{\n        let next = match cell.get() {{\n            Some(n) => n + {addend:?}_f64,\n            None => f64::NAN,\n        }};\n        cell.set(Some(next));\n        return w3cos_core::Value::Undefined;\n    }}\n"
    )
}

fn emit_shared_number_load(plan: &EscapePlan, dst: u32, binding: u32) -> String {
    if let Some(slot) = num_slot(plan, dst) {
        format!(
            "self.num_regs[{slot}] = self.num_cells.get(&{binding}).and_then(|cell| cell.get()).unwrap_or(f64::NAN);"
        )
    } else {
        let register = val_index(plan, dst);
        format!(
            "self.registers[{register}] = match self.num_cells.get(&{binding}).and_then(|cell| cell.get()) {{ Some(n) => w3cos_core::Value::Number(n), None => w3cos_core::Value::Undefined }};"
        )
    }
}

fn emit_shared_number_store(plan: &EscapePlan, binding: u32, value: u32) -> String {
    let expr = if let Some(slot) = num_slot(plan, value) {
        format!("self.num_regs[{slot}]")
    } else {
        format!("{}.to_number()", emit_boxed_value(plan, value))
    };
    format!("if let Some(cell) = self.num_cells.get(&{binding}) {{ cell.set(Some({expr})); }}")
}

fn emit_num_cells(ids: &HashSet<u32>) -> (String, String, String) {
    if ids.is_empty() {
        return (String::new(), String::new(), String::new());
    }
    let mut ordered = ids.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    let mut local = String::from(
        "    let mut num_cells: std::collections::HashMap<u32, std::rc::Rc<std::cell::Cell<Option<f64>>>> = std::collections::HashMap::new();\n",
    );
    for id in ordered {
        local.push_str(&format!(
            "    num_cells.insert({id}, std::rc::Rc::new(std::cell::Cell::new(None)));\n"
        ));
    }
    (
        "    num_cells: std::collections::HashMap<u32, std::rc::Rc<std::cell::Cell<Option<f64>>>>,\n"
            .into(),
        local,
        "        num_cells,\n".into(),
    )
}

/// Whether outer binding `id` is named `eval`. Without module context any
/// capture may be `eval`, so the answer is conservatively `true`.
fn is_eval_binding(id: u32) -> bool {
    MODULE_FUNCTIONS.with(|functions| {
        let functions = functions.borrow();
        if functions.is_empty() {
            return true;
        }
        EVAL_BINDINGS.with(|cache| {
            let mut cache = cache.borrow_mut();
            let stale = cache
                .as_ref()
                .is_none_or(|(count, _)| *count != functions.len());
            if stale {
                let ids = functions
                    .iter()
                    .flat_map(|function| &function.bindings)
                    .filter(|binding| binding.name == "eval")
                    .map(|binding| binding.id.0)
                    .collect();
                *cache = Some((functions.len(), ids));
            }
            cache.as_ref().is_some_and(|(_, ids)| ids.contains(&id))
        })
    })
}

fn analyze_function(function: &Function) -> EscapePlan {
    let constants = NUMERIC_CONSTANTS.with(|constants| constants.borrow().clone());
    let numeric_bindings = numeric_local_bindings(function, &constants);
    let len = function.registers as usize;
    let mut classes = vec![RegClass::Bottom; len];
    let mut changed = true;
    while changed {
        changed = false;
        let before = classes.clone();
        for block in &function.blocks {
            for instruction in &block.instructions {
                apply_register_def(&mut classes, instruction, &numeric_bindings, &constants);
            }
        }
        if classes != before {
            changed = true;
        }
    }
    let mut number = HashSet::new();
    let mut bools = HashSet::new();
    let mut num_of = vec![None; len];
    let mut bool_of = vec![None; len];
    let mut val_of = vec![None; len];
    let mut n_num = 0;
    let mut n_bool = 0;
    let mut n_val = 0;
    for (index, class) in classes.iter().enumerate() {
        match class {
            RegClass::Number => {
                number.insert(index as u32);
                num_of[index] = Some(n_num);
                n_num += 1;
            }
            RegClass::Bool => {
                bools.insert(index as u32);
                bool_of[index] = Some(n_bool);
                n_bool += 1;
            }
            _ => {
                val_of[index] = Some(n_val);
                n_val += 1;
            }
        }
    }
    let mut num_binding_of = HashMap::new();
    let mut n_num_bindings = 0;
    let mut ordered = numeric_bindings.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    for id in ordered {
        num_binding_of.insert(id, n_num_bindings);
        n_num_bindings += 1;
    }
    EscapePlan {
        bools,
        num_of,
        bool_of,
        val_of,
        n_num,
        n_bool,
        n_val,
        local_allocs: crate::alloc_escape::analyze(
            function,
            &|register| number.contains(&register),
            &is_eval_binding,
        )
        .local_registers,
        num_binding_of,
        n_num_bindings,
        constants,
        number,
    }
}

/// `let`/`var` bindings stay writable from every nested closure, including
/// closures that only re-capture a binding declared in an outer function.
/// Those outer declarations are not copied into the inner function's binding
/// list, so a local lookup would treat the cell as const and emit a getter
/// with an undefined setter.
fn binding_is_mutable(function: &Function, id: u32) -> bool {
    if let Some(binding) = function.bindings.iter().find(|binding| binding.id.0 == id) {
        return binding.mutable;
    }
    MODULE_FUNCTIONS.with(|functions| {
        functions.borrow().iter().any(|other| {
            other
                .bindings
                .iter()
                .any(|binding| binding.id.0 == id && binding.mutable)
        })
    })
}

fn captured_binding_ids(function: &Function) -> HashSet<u32> {
    let mut captured = HashSet::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            if let Instruction::CreateClosure { captures, .. } = instruction {
                captured.extend(captures.iter().map(|binding| binding.0));
            }
        }
    }
    captured
}

fn numeric_local_bindings(function: &Function, constants: &HashMap<u32, f64>) -> HashSet<u32> {
    let captured = captured_binding_ids(function);
    let mut reserved = HashSet::new();
    reserved.extend(function.parameters.iter().map(|binding| binding.0));
    reserved.extend(function.captures.iter().map(|binding| binding.0));
    if let Some(binding) = function.rest_parameter {
        reserved.insert(binding.0);
    }
    if let Some(binding) = function.arguments_binding {
        reserved.insert(binding.0);
    }
    if let Some(binding) = function.this_binding {
        reserved.insert(binding.0);
    }
    let eligible = function
        .bindings
        .iter()
        .filter(|binding| {
            matches!(
                binding.kind,
                BindingKind::Var | BindingKind::Let | BindingKind::Const
            ) && !captured.contains(&binding.id.0)
                && !reserved.contains(&binding.id.0)
        })
        .map(|binding| binding.id.0)
        .collect::<HashSet<_>>();
    let mut numeric = eligible.clone();
    loop {
        let classes = register_classes(function, &numeric, constants);
        let number_regs = classes
            .iter()
            .enumerate()
            .filter(|(_, class)| **class == RegClass::Number)
            .map(|(index, _)| index as u32)
            .collect::<HashSet<_>>();
        let dominated = loads_after_numeric_store(function, &numeric, &number_regs);
        let mut next = HashSet::new();
        for id in &eligible {
            if dominated.contains(id) && stores_are_numbers(function, *id, &number_regs) {
                next.insert(*id);
            }
        }
        if next == numeric {
            return numeric;
        }
        numeric = next;
    }
}

fn register_classes(
    function: &Function,
    numeric: &HashSet<u32>,
    constants: &HashMap<u32, f64>,
) -> Vec<RegClass> {
    let mut classes = vec![RegClass::Bottom; function.registers as usize];
    let mut changed = true;
    while changed {
        changed = false;
        let before = classes.clone();
        for block in &function.blocks {
            for instruction in &block.instructions {
                apply_register_def(&mut classes, instruction, numeric, constants);
            }
        }
        if classes != before {
            changed = true;
        }
    }
    classes
}

fn stores_are_numbers(function: &Function, binding: u32, number_regs: &HashSet<u32>) -> bool {
    let mut saw_store = false;
    for block in &function.blocks {
        for instruction in &block.instructions {
            let source = match instruction {
                Instruction::InitializeBinding { binding: id, value }
                | Instruction::StoreBinding { binding: id, value }
                    if id.0 == binding =>
                {
                    Some(value.0)
                }
                _ => None,
            };
            let Some(source) = source else {
                continue;
            };
            saw_store = true;
            if !number_regs.contains(&source) {
                return false;
            }
        }
    }
    saw_store
}

fn loads_after_numeric_store(
    function: &Function,
    candidates: &HashSet<u32>,
    number_regs: &HashSet<u32>,
) -> HashSet<u32> {
    let mut succs: HashMap<w3cos_ir::BlockId, Vec<w3cos_ir::BlockId>> = HashMap::new();
    let mut preds: HashMap<w3cos_ir::BlockId, Vec<w3cos_ir::BlockId>> = HashMap::new();
    for block in &function.blocks {
        let targets = match block.instructions.last() {
            Some(Instruction::Jump { target }) => vec![*target],
            Some(Instruction::Branch {
                then_block,
                else_block,
                ..
            }) => {
                if then_block == else_block {
                    vec![*then_block]
                } else {
                    vec![*then_block, *else_block]
                }
            }
            _ => Vec::new(),
        };
        for target in targets {
            succs.entry(block.id).or_default().push(target);
            preds.entry(target).or_default().push(block.id);
        }
    }
    let mut out: HashMap<w3cos_ir::BlockId, HashSet<u32>> = HashMap::new();
    let mut changed = true;
    while changed {
        changed = false;
        for block in &function.blocks {
            let mut state = if block.id == function.entry {
                HashSet::new()
            } else {
                let mut incoming: Option<HashSet<u32>> = None;
                let mut saw_pred = false;
                for pred in preds.get(&block.id).into_iter().flatten() {
                    let Some(pred_out) = out.get(pred) else {
                        continue;
                    };
                    saw_pred = true;
                    incoming = Some(match incoming {
                        None => pred_out.clone(),
                        Some(state) => state.intersection(pred_out).copied().collect(),
                    });
                }
                if !saw_pred {
                    continue;
                }
                incoming.unwrap_or_default()
            };
            for instruction in &block.instructions {
                match instruction {
                    Instruction::InitializeBinding { binding, value }
                    | Instruction::StoreBinding { binding, value }
                        if candidates.contains(&binding.0) =>
                    {
                        if number_regs.contains(&value.0) {
                            state.insert(binding.0);
                        } else {
                            state.remove(&binding.0);
                        }
                    }
                    _ => {}
                }
            }
            if out.get(&block.id) != Some(&state) {
                out.insert(block.id, state);
                changed = true;
            }
        }
    }
    let mut unsafe_loads = HashSet::new();
    for block in &function.blocks {
        if !out.contains_key(&block.id) && block.id != function.entry {
            for instruction in &block.instructions {
                if let Instruction::LoadBinding { binding, .. } = instruction {
                    if candidates.contains(&binding.0) {
                        unsafe_loads.insert(binding.0);
                    }
                }
            }
            continue;
        }
        let mut state = if block.id == function.entry {
            HashSet::new()
        } else {
            let mut incoming: Option<HashSet<u32>> = None;
            for pred in preds.get(&block.id).into_iter().flatten() {
                let Some(pred_out) = out.get(pred) else {
                    continue;
                };
                incoming = Some(match incoming {
                    None => pred_out.clone(),
                    Some(state) => state.intersection(pred_out).copied().collect(),
                });
            }
            incoming.unwrap_or_default()
        };
        for instruction in &block.instructions {
            if let Instruction::LoadBinding { binding, .. } = instruction {
                if candidates.contains(&binding.0) && !state.contains(&binding.0) {
                    unsafe_loads.insert(binding.0);
                }
            }
            if let Instruction::InitializeBinding { binding, value }
            | Instruction::StoreBinding { binding, value } = instruction
            {
                if candidates.contains(&binding.0) {
                    if number_regs.contains(&value.0) {
                        state.insert(binding.0);
                    } else {
                        state.remove(&binding.0);
                    }
                }
            }
        }
    }
    for region in &function.exception_regions {
        for block_id in region.catch_block.into_iter().chain(region.finally_block) {
            let Some(block) = function.blocks.iter().find(|block| block.id == block_id) else {
                continue;
            };
            for instruction in &block.instructions {
                if let Instruction::LoadBinding { binding, .. } = instruction {
                    if candidates.contains(&binding.0) {
                        unsafe_loads.insert(binding.0);
                    }
                }
            }
        }
    }
    candidates
        .iter()
        .copied()
        .filter(|id| !unsafe_loads.contains(id))
        .collect()
}

fn module_numeric_constants(functions: &[Function]) -> HashMap<u32, f64> {
    let mut known = HashMap::new();
    loop {
        let mut stores: HashMap<u32, Vec<Option<f64>>> = HashMap::new();
        for function in functions {
            for block in &function.blocks {
                let mut regs: HashMap<u32, f64> = HashMap::new();
                for instruction in &block.instructions {
                    match instruction {
                        Instruction::LoadConstant {
                            dst,
                            value: w3cos_ir::Constant::Number(value),
                        } => {
                            regs.insert(dst.0, *value);
                        }
                        Instruction::LoadBinding { dst, binding } => {
                            if let Some(value) = known.get(&binding.0).copied() {
                                regs.insert(dst.0, value);
                            } else {
                                regs.remove(&dst.0);
                            }
                        }
                        Instruction::Move { dst, src } => {
                            if let Some(value) = regs.get(&src.0).copied() {
                                regs.insert(dst.0, value);
                            } else {
                                regs.remove(&dst.0);
                            }
                        }
                        Instruction::InitializeBinding { binding, value }
                        | Instruction::StoreBinding { binding, value } => {
                            stores
                                .entry(binding.0)
                                .or_default()
                                .push(regs.get(&value.0).copied());
                        }
                        other => {
                            if let Some(dst) = instruction_dst(other) {
                                regs.remove(&dst);
                            }
                        }
                    }
                }
            }
        }
        let next = stores
            .into_iter()
            .filter_map(|(id, values)| {
                if values.len() == 1 {
                    values[0].map(|value| (id, value))
                } else {
                    None
                }
            })
            .collect::<HashMap<_, _>>();
        if next == known {
            return known;
        }
        known = next;
    }
}

struct NativeLoop {
    body: w3cos_ir::BlockId,
    exit: w3cos_ir::BlockId,
    then_is_body: bool,
}

fn find_native_loop(
    header: w3cos_ir::BlockId,
    instructions: &[Instruction],
    blocks: &[(w3cos_ir::BlockId, Vec<Instruction>)],
    function: &Function,
) -> Option<NativeLoop> {
    let Instruction::Branch {
        then_block,
        else_block,
        ..
    } = instructions.last()?
    else {
        return None;
    };
    let mut preds: HashMap<w3cos_ir::BlockId, Vec<w3cos_ir::BlockId>> = HashMap::new();
    for (id, insts) in blocks {
        match insts.last() {
            Some(Instruction::Jump { target }) => preds.entry(*target).or_default().push(*id),
            Some(Instruction::Branch {
                then_block,
                else_block,
                ..
            }) => {
                preds.entry(*then_block).or_default().push(*id);
                if then_block != else_block {
                    preds.entry(*else_block).or_default().push(*id);
                }
            }
            _ => {}
        }
    }
    let by_id = blocks
        .iter()
        .map(|(id, insts)| (*id, insts.as_slice()))
        .collect::<HashMap<_, _>>();
    for (body, exit, then_is_body) in [
        (*then_block, *else_block, true),
        (*else_block, *then_block, false),
    ] {
        if body == header {
            continue;
        }
        let Some(body_insts) = by_id.get(&body) else {
            continue;
        };
        let Some(Instruction::Jump { target }) = body_insts.last() else {
            continue;
        };
        if *target != header {
            continue;
        }
        if preds.get(&body).map(Vec::as_slice) != Some(&[header]) {
            continue;
        }
        if exception_target_for_block(function, header).is_some()
            || exception_target_for_block(function, body).is_some()
        {
            continue;
        }
        let interrupted = |inst: &Instruction| {
            matches!(
                inst,
                Instruction::Jump { .. }
                    | Instruction::Branch { .. }
                    | Instruction::Return { .. }
                    | Instruction::Throw { .. }
                    | Instruction::Await { .. }
                    | Instruction::Yield { .. }
                    | Instruction::YieldDelegate { .. }
            )
        };
        if instructions[..instructions.len() - 1]
            .iter()
            .any(interrupted)
            || body_insts[..body_insts.len() - 1].iter().any(interrupted)
        {
            continue;
        }
        return Some(NativeLoop {
            body,
            exit,
            then_is_body,
        });
    }
    None
}

fn emit_branch_condition(plan: &EscapePlan, condition: u32) -> String {
    if known_bool(plan, condition) {
        emit_bool_operand(plan, condition)
    } else if num_slot(plan, condition).is_some() {
        let slot = num_index(plan, condition);
        format!("(self.num_regs[{slot}] != 0.0_f64 && !self.num_regs[{slot}].is_nan())")
    } else {
        format!("{}.to_bool()", emit_boxed_value(plan, condition))
    }
}

fn promote_unboxed_locals(code: &str) -> Option<(String, String, String)> {
    let mut rewritten = String::with_capacity(code.len());
    let bytes = code.as_bytes();
    let mut index = 0;
    let mut nums = Vec::new();
    let mut bools = Vec::new();
    let mut binds = Vec::new();
    while index < bytes.len() {
        let rest = &code[index..];
        let replaced = ["self.num_bindings[", "self.num_regs[", "self.bool_regs["]
            .iter()
            .find_map(|prefix| {
                if !rest.starts_with(prefix) {
                    return None;
                }
                let after = index + prefix.len();
                let end = code[after..].find(']').map(|offset| after + offset)?;
                let slot: u32 = code[after..end].parse().ok()?;
                let name = if prefix.starts_with("self.num_bindings") {
                    binds.push(slot);
                    format!("nb_{slot}")
                } else if prefix.starts_with("self.num_regs") {
                    nums.push(slot);
                    format!("nr_{slot}")
                } else {
                    bools.push(slot);
                    format!("br_{slot}")
                };
                Some((name, end + 1))
            });
        if let Some((name, next)) = replaced {
            rewritten.push_str(&name);
            index = next;
        } else {
            rewritten.push(bytes[index] as char);
            index += code[index..]
                .chars()
                .next()
                .map(|ch| ch.len_utf8())
                .unwrap_or(1);
        }
    }
    if rewritten.contains("self.") {
        return None;
    }
    nums.sort_unstable();
    nums.dedup();
    bools.sort_unstable();
    bools.dedup();
    binds.sort_unstable();
    binds.dedup();
    let mut prologue = String::new();
    let mut epilogue = String::new();
    for slot in nums {
        prologue.push_str(&format!(
            "let mut nr_{slot}: f64 = self.num_regs[{slot}];\n"
        ));
        epilogue.push_str(&format!("self.num_regs[{slot}] = nr_{slot};\n"));
    }
    for slot in bools {
        prologue.push_str(&format!(
            "let mut br_{slot}: bool = self.bool_regs[{slot}];\n"
        ));
        epilogue.push_str(&format!("self.bool_regs[{slot}] = br_{slot};\n"));
    }
    for slot in binds {
        prologue.push_str(&format!(
            "let mut nb_{slot}: f64 = self.num_bindings[{slot}];\n"
        ));
        epilogue.push_str(&format!("self.num_bindings[{slot}] = nb_{slot};\n"));
    }
    Some((prologue, rewritten, epilogue))
}

fn num_slot(plan: &EscapePlan, ir: u32) -> Option<u32> {
    plan.num_of.get(ir as usize).copied().flatten()
}

fn bool_slot(plan: &EscapePlan, ir: u32) -> Option<u32> {
    plan.bool_of.get(ir as usize).copied().flatten()
}

fn val_slot(plan: &EscapePlan, ir: u32) -> Option<u32> {
    plan.val_of.get(ir as usize).copied().flatten()
}

fn num_index(plan: &EscapePlan, ir: u32) -> u32 {
    num_slot(plan, ir).unwrap_or_else(|| panic!("missing number slot for IR register {ir}"))
}

fn bool_index(plan: &EscapePlan, ir: u32) -> u32 {
    bool_slot(plan, ir).unwrap_or_else(|| panic!("missing bool slot for IR register {ir}"))
}

fn val_index(plan: &EscapePlan, ir: u32) -> u32 {
    val_slot(plan, ir).unwrap_or_else(|| panic!("missing value slot for IR register {ir}"))
}

fn emit_exception_propagation(
    exception_target: Option<(w3cos_ir::Register, w3cos_ir::BlockId)>,
    mode: EmissionMode,
    plan: &EscapePlan,
) -> String {
    if let Some((exception, target)) = exception_target {
        return format!(
            "{{ {}; self.block = {}; continue 'drive; }}",
            store_boxed(plan, exception.0, "__w3cos_exception"),
            target.0,
        );
    }
    match mode {
        EmissionMode::Sync | EmissionMode::Async => {
            "{ return Err(__w3cos_exception); }".to_string()
        }
        EmissionMode::Generator | EmissionMode::AsyncGenerator => {
            "{ return w3cos_core::throw_value(__w3cos_exception); }".to_string()
        }
    }
}

fn emit_completion_match(
    completion_expr: &str,
    dst_store: &str,
    exception_target: Option<(w3cos_ir::Register, w3cos_ir::BlockId)>,
    mode: EmissionMode,
    plan: &EscapePlan,
) -> String {
    format!(
        "match {completion_expr} {{ Ok(__w3cos_value) => {{ {dst_store} }} Err(__w3cos_exception) => {} }}",
        emit_exception_propagation(exception_target, mode, plan),
    )
}

fn emit_caught_stmt(
    stmt: &str,
    exception_target: Option<(w3cos_ir::Register, w3cos_ir::BlockId)>,
    plan: &EscapePlan,
) -> String {
    let Some((exception, target)) = exception_target else {
        return stmt.to_string();
    };
    format!(
        "if let Err(__w3cos_exception) = w3cos_core::catch_js(|| {{ {stmt} w3cos_core::Value::Undefined }}) {{ {}; self.block = {}; continue 'drive; }}",
        store_boxed(plan, exception.0, "__w3cos_exception"),
        target.0,
    )
}

fn emit_caught_value(
    expr: &str,
    dst_store: &str,
    exception_target: Option<(w3cos_ir::Register, w3cos_ir::BlockId)>,
    mode: EmissionMode,
    plan: &EscapePlan,
) -> String {
    match exception_target {
        None => match mode {
            EmissionMode::Sync | EmissionMode::Async => emit_completion_match(
                &format!("w3cos_core::catch_js(|| {expr})"),
                dst_store,
                None,
                mode,
                plan,
            ),
            EmissionMode::Generator | EmissionMode::AsyncGenerator => {
                dst_store.replace("__w3cos_value", expr)
            }
        },
        Some(_) => emit_completion_match(
            &format!("w3cos_core::catch_js(|| {expr})"),
            dst_store,
            exception_target,
            mode,
            plan,
        ),
    }
}

fn is_call_like(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::Call { .. }
            | Instruction::CallWithArguments { .. }
            | Instruction::CallMethod { .. }
            | Instruction::CallMethodWithArguments { .. }
            | Instruction::Construct { .. }
            | Instruction::ConstructWithArguments { .. }
    )
}

fn handles_own_exceptions(instruction: &Instruction) -> bool {
    is_call_like(instruction)
        || matches!(
            instruction,
            Instruction::LoadBinding { .. } | Instruction::StoreBinding { .. }
        )
}

fn store_boxed(plan: &EscapePlan, ir: u32, expr: &str) -> String {
    if let Some(slot) = num_slot(plan, ir) {
        format!("self.num_regs[{slot}] = ({expr}).to_number()")
    } else if let Some(slot) = bool_slot(plan, ir) {
        format!("self.bool_regs[{slot}] = ({expr}).to_bool()")
    } else {
        format!("self.registers[{}] = {expr}", val_index(plan, ir))
    }
}

fn class_of(classes: &[RegClass], register: Register) -> RegClass {
    classes
        .get(register.0 as usize)
        .copied()
        .unwrap_or(RegClass::Unknown)
}

fn def_class(classes: &mut [RegClass], register: Register, class: RegClass) {
    let index = register.0 as usize;
    if index < classes.len() {
        classes[index] = join_class(classes[index], class);
    }
}

fn apply_register_def(
    classes: &mut [RegClass],
    instruction: &Instruction,
    numeric_bindings: &HashSet<u32>,
    constants: &HashMap<u32, f64>,
) {
    match instruction {
        Instruction::LoadConstant { dst, value } => {
            let class = match value {
                Constant::Number(_) => RegClass::Number,
                Constant::Bool(_) => RegClass::Bool,
                Constant::String(_) => RegClass::String,
                Constant::Undefined | Constant::Null => RegClass::Unknown,
            };
            def_class(classes, *dst, class);
        }
        Instruction::Move { dst, src } => {
            def_class(classes, *dst, class_of(classes, *src));
        }
        Instruction::Add { dst, lhs, rhs } => {
            // JS `+` concatenates when either side may be a string.
            let class = if class_of(classes, *lhs) == RegClass::Number
                && class_of(classes, *rhs) == RegClass::Number
            {
                RegClass::Number
            } else {
                RegClass::Unknown
            };
            def_class(classes, *dst, class);
        }
        Instruction::Binary {
            dst,
            operator,
            lhs,
            rhs,
        } => {
            let both_number = class_of(classes, *lhs) == RegClass::Number
                && class_of(classes, *rhs) == RegClass::Number;
            let class = match operator {
                BinaryOperator::Subtract
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Remainder
                | BinaryOperator::Exponentiate
                | BinaryOperator::LeftShift
                | BinaryOperator::SignedRightShift
                | BinaryOperator::UnsignedRightShift
                | BinaryOperator::BitwiseOr
                | BinaryOperator::BitwiseXor
                | BinaryOperator::BitwiseAnd
                    if both_number =>
                {
                    RegClass::Number
                }
                BinaryOperator::LessThan
                | BinaryOperator::LessThanOrEqual
                | BinaryOperator::GreaterThan
                | BinaryOperator::GreaterThanOrEqual
                | BinaryOperator::AbstractEqual
                | BinaryOperator::StrictEqual
                    if both_number =>
                {
                    RegClass::Bool
                }
                _ => RegClass::Unknown,
            };
            def_class(classes, *dst, class);
        }
        Instruction::Unary {
            dst,
            operator,
            value,
        } => {
            let class = match operator {
                UnaryOperator::Negate if class_of(classes, *value) == RegClass::Number => {
                    RegClass::Number
                }
                UnaryOperator::TypeOf => RegClass::String,
                _ => RegClass::Unknown,
            };
            def_class(classes, *dst, class);
        }
        Instruction::LoadBinding { dst, binding }
            if numeric_bindings.contains(&binding.0) || constants.contains_key(&binding.0) =>
        {
            def_class(classes, *dst, RegClass::Number);
        }
        Instruction::LoadBinding { dst, .. }
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
        | Instruction::YieldDelegate { dst, .. } => def_class(classes, *dst, RegClass::Unknown),
        _ => {}
    }
}

fn emit_unboxed_reg_storage(plan: &EscapePlan) -> (String, String) {
    let mut fields = String::new();
    let mut inits = String::new();
    if plan.n_num > 0 {
        if (plan.n_num as usize) <= INLINE_FRAME {
            fields.push_str(&format!("    num_regs: [f64; {}],\n", plan.n_num));
            inits.push_str(&format!("        num_regs: [0.0_f64; {}],\n", plan.n_num));
        } else {
            fields.push_str("    num_regs: Vec<f64>,\n");
            inits.push_str(&format!(
                "        num_regs: vec![0.0_f64; {}],\n",
                plan.n_num
            ));
        }
    }
    if plan.n_bool > 0 {
        if (plan.n_bool as usize) <= INLINE_FRAME {
            fields.push_str(&format!("    bool_regs: [bool; {}],\n", plan.n_bool));
            inits.push_str(&format!("        bool_regs: [false; {}],\n", plan.n_bool));
        } else {
            fields.push_str("    bool_regs: Vec<bool>,\n");
            inits.push_str(&format!(
                "        bool_regs: vec![false; {}],\n",
                plan.n_bool
            ));
        }
    }
    if plan.n_num_bindings > 0 {
        if (plan.n_num_bindings as usize) <= INLINE_FRAME {
            fields.push_str(&format!(
                "    num_bindings: [f64; {}],\n",
                plan.n_num_bindings
            ));
            inits.push_str(&format!(
                "        num_bindings: [0.0_f64; {}],\n",
                plan.n_num_bindings
            ));
        } else {
            fields.push_str("    num_bindings: Vec<f64>,\n");
            inits.push_str(&format!(
                "        num_bindings: vec![0.0_f64; {}],\n",
                plan.n_num_bindings
            ));
        }
    }
    (fields, inits)
}

fn num_binding_slot(plan: &EscapePlan, binding: u32) -> Option<u32> {
    plan.num_binding_of.get(&binding).copied()
}

fn emit_numeric_binding_store(plan: &EscapePlan, binding: u32, value: u32) -> String {
    let Some(slot) = num_binding_slot(plan, binding) else {
        return String::new();
    };
    if let Some(num) = num_slot(plan, value) {
        format!("self.num_bindings[{slot}] = self.num_regs[{num}];")
    } else if let Some(constant) = plan.constants.get(&value) {
        let _ = constant;
        format!(
            "self.num_bindings[{slot}] = {}.to_number();",
            emit_boxed_value(plan, value)
        )
    } else {
        format!(
            "self.num_bindings[{slot}] = {}.to_number();",
            emit_boxed_value(plan, value)
        )
    }
}

fn emit_numeric_binding_load(plan: &EscapePlan, dst: u32, slot: u32) -> String {
    if let Some(num) = num_slot(plan, dst) {
        format!("self.num_regs[{num}] = self.num_bindings[{slot}];")
    } else if let Some(bool_slot) = bool_slot(plan, dst) {
        format!(
            "self.bool_regs[{bool_slot}] = self.num_bindings[{slot}] != 0.0_f64 && !self.num_bindings[{slot}].is_nan();"
        )
    } else {
        format!(
            "self.registers[{}] = w3cos_core::Value::Number(self.num_bindings[{slot}]);",
            val_index(plan, dst)
        )
    }
}

fn emit_constant_binding_load(plan: &EscapePlan, dst: u32, value: f64) -> String {
    if let Some(num) = num_slot(plan, dst) {
        format!("self.num_regs[{num}] = {value:?}_f64;")
    } else if let Some(bool_slot) = bool_slot(plan, dst) {
        let truth = value != 0.0 && !value.is_nan();
        format!("self.bool_regs[{bool_slot}] = {truth};")
    } else {
        format!(
            "self.registers[{}] = w3cos_core::Value::Number({value:?});",
            val_index(plan, dst)
        )
    }
}

fn known_number(plan: &EscapePlan, reaching: &HashMap<u32, f64>, register: u32) -> bool {
    plan.number.contains(&register) || reaching.contains_key(&register)
}

fn known_bool(plan: &EscapePlan, register: u32) -> bool {
    plan.bools.contains(&register)
}

fn emit_f64_operand(plan: &EscapePlan, reaching: &HashMap<u32, f64>, register: u32) -> String {
    if let Some(value) = reaching.get(&register) {
        format!("{value:?}_f64")
    } else if let Some(slot) = num_slot(plan, register) {
        format!("self.num_regs[{slot}]")
    } else if let Some(slot) = bool_slot(plan, register) {
        format!("if self.bool_regs[{slot}] {{ 1.0_f64 }} else {{ 0.0_f64 }}")
    } else {
        format!("self.registers[{}].to_number()", val_index(plan, register))
    }
}

fn emit_bool_operand(plan: &EscapePlan, register: u32) -> String {
    if let Some(slot) = bool_slot(plan, register) {
        format!("self.bool_regs[{slot}]")
    } else if let Some(slot) = num_slot(plan, register) {
        format!("(self.num_regs[{slot}] != 0.0_f64 && !self.num_regs[{slot}].is_nan())")
    } else {
        format!("{}.to_bool()", emit_boxed_value(plan, register))
    }
}

fn emit_boxed_value(plan: &EscapePlan, register: u32) -> String {
    if let Some(slot) = num_slot(plan, register) {
        format!("w3cos_core::Value::Number(self.num_regs[{slot}])")
    } else if let Some(slot) = bool_slot(plan, register) {
        format!("w3cos_core::Value::Bool(self.bool_regs[{slot}])")
    } else {
        format!("self.registers[{}].clone()", val_index(plan, register))
    }
}

/// Operand for a `&Value` parameter. A boxed register is borrowed in place
/// (`self.registers[i]`, written after a `&` or as a method receiver)
/// instead of cloned into a temporary: the clone was one slot retain plus
/// one release per use. Rust's borrow checker rejects any emission where
/// the register could change while borrowed, so this cannot free early.
/// Unboxed numbers / booleans still build a temporary `Value`.
fn emit_boxed_ref(plan: &EscapePlan, register: u32) -> String {
    if let Some(slot) = val_slot(plan, register) {
        format!("self.registers[{slot}]")
    } else {
        emit_boxed_value(plan, register)
    }
}

/// Operand for a by-value `Value` parameter: on the register's last use,
/// when no other operand of the instruction reads it, the value is moved
/// out (into a `let` hoisted before the statement, so it never overlaps a
/// borrow of `self.registers`); otherwise cloned. Returns `(hoist, expr)`.
fn emit_owned_operand(
    plan: &EscapePlan,
    last_uses: &HashSet<u32>,
    reads: &[u32],
    register: u32,
) -> (String, String) {
    let once = reads.iter().filter(|read| **read == register).count() == 1;
    match val_slot(plan, register) {
        Some(slot) if once && last_uses.contains(&register) => (
            format!(
                "let __w3cos_owned_{register} = std::mem::replace(&mut self.registers[{slot}], w3cos_core::Value::Undefined); "
            ),
            format!("__w3cos_owned_{register}"),
        ),
        _ => (String::new(), emit_boxed_value(plan, register)),
    }
}

/// Wrap `statement` in a block after its hoisted moves, if any.
fn with_hoisted(hoisted: &str, statement: String) -> String {
    if hoisted.is_empty() {
        statement
    } else {
        format!("{{ {hoisted}{statement} }}")
    }
}

fn emit_boxed_use(plan: &EscapePlan, register: u32, take: bool) -> String {
    if take {
        if let Some(slot) = val_slot(plan, register) {
            return format!(
                "std::mem::replace(&mut self.registers[{slot}], w3cos_core::Value::Undefined)"
            );
        }
    }
    emit_boxed_value(plan, register)
}

struct ValueUseEmitter<'a> {
    plan: &'a EscapePlan,
    remaining: HashMap<u32, usize>,
}

impl<'a> ValueUseEmitter<'a> {
    fn new(
        plan: &'a EscapePlan,
        last_uses: &HashSet<u32>,
        reads: impl IntoIterator<Item = u32>,
    ) -> Self {
        let mut remaining = HashMap::new();
        for register in reads {
            if last_uses.contains(&register) && val_slot(plan, register).is_some() {
                *remaining.entry(register).or_insert(0) += 1;
            }
        }
        Self { plan, remaining }
    }

    /// No operand of this instruction is moved out, so `&`-positions may
    /// borrow registers in place without conflicting with a `&mut`.
    fn takes_nothing(&self) -> bool {
        self.remaining.is_empty()
    }

    /// Operand in a `&Value` position: borrowed in place when the
    /// instruction moves nothing out, else as [`Self::emit`].
    fn emit_ref(&mut self, register: u32) -> String {
        if self.takes_nothing() {
            emit_boxed_ref(self.plan, register)
        } else {
            self.emit(register)
        }
    }

    fn emit(&mut self, register: u32) -> String {
        let take = self
            .remaining
            .get_mut(&register)
            .map(|count| {
                *count -= 1;
                *count == 0
            })
            .unwrap_or(false);
        emit_boxed_use(self.plan, register, take)
    }
}

fn write_f64(plan: &EscapePlan, dst: u32, expr: &str) -> String {
    if let Some(slot) = num_slot(plan, dst) {
        format!("self.num_regs[{slot}] = {expr};")
    } else {
        format!(
            "self.registers[{}] = w3cos_core::Value::Number({expr});",
            val_index(plan, dst)
        )
    }
}

fn write_bool(plan: &EscapePlan, dst: u32, expr: &str) -> String {
    if let Some(slot) = bool_slot(plan, dst) {
        format!("self.bool_regs[{slot}] = {expr};")
    } else {
        format!(
            "self.registers[{}] = w3cos_core::Value::Bool({expr});",
            val_index(plan, dst)
        )
    }
}

fn instruction_dst(instruction: &Instruction) -> Option<u32> {
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
        | Instruction::YieldDelegate { dst, .. } => Some(dst.0),
        _ => None,
    }
}

fn reaching_number_constants(instructions: &[Instruction]) -> Vec<HashMap<u32, f64>> {
    let mut current = HashMap::new();
    let mut output = Vec::with_capacity(instructions.len());
    for instruction in instructions {
        output.push(current.clone());
        match instruction {
            Instruction::LoadConstant {
                dst,
                value: Constant::Number(value),
            } => {
                current.insert(dst.0, *value);
            }
            other => {
                if let Some(dst) = instruction_dst(other) {
                    current.remove(&dst);
                }
            }
        }
    }
    output
}

const INLINE_FRAME: usize = 8;

#[derive(Clone, Copy)]
enum SlotStorage {
    /// Binding ids are exactly `0..len`.
    Dense(usize),
    /// At most [`INLINE_FRAME`] sparse ids, packed into a stack array.
    /// The array is the original ids; the length is how many are live.
    Packed([u32; INLINE_FRAME], u8),
    Map,
}

fn slot_index(storage: SlotStorage, id: u32) -> usize {
    match storage {
        SlotStorage::Packed(ids, len) => ids[..len as usize]
            .iter()
            .position(|candidate| *candidate == id)
            .expect("packed binding id"),
        SlotStorage::Dense(_) | SlotStorage::Map => id as usize,
    }
}

fn slot_storage(function: &Function) -> SlotStorage {
    let mut ids = Vec::new();
    for binding in &function.bindings {
        ids.push(binding.id.0);
    }
    for binding in &function.captures {
        ids.push(binding.0);
    }
    for binding in &function.parameters {
        ids.push(binding.0);
    }
    if let Some(binding) = function.rest_parameter {
        ids.push(binding.0);
    }
    if let Some(binding) = function.arguments_binding {
        ids.push(binding.0);
    }
    if let Some(binding) = function.this_binding {
        ids.push(binding.0);
    }
    ids.sort_unstable();
    ids.dedup();
    match ids.as_slice() {
        [] => SlotStorage::Dense(0),
        [first, ..] if *first == 0 && ids.len() == (*ids.last().unwrap() as usize) + 1 => {
            SlotStorage::Dense(ids.len())
        }
        _ if ids.len() <= INLINE_FRAME => {
            let mut packed = [0u32; INLINE_FRAME];
            for (index, id) in ids.iter().enumerate() {
                packed[index] = *id;
            }
            SlotStorage::Packed(packed, ids.len() as u8)
        }
        _ => SlotStorage::Map,
    }
}

fn emit_registers_field(n: u32) -> String {
    if (n as usize) <= INLINE_FRAME {
        format!("    registers: [w3cos_core::Value; {n}],\n")
    } else {
        "    registers: Vec<w3cos_core::Value>,\n".to_string()
    }
}

fn emit_registers_init(n: u32) -> String {
    if (n as usize) <= INLINE_FRAME {
        "        registers: std::array::from_fn(|_| w3cos_core::Value::Undefined),\n".to_string()
    } else {
        format!("        registers: vec![w3cos_core::Value::Undefined; {n}],\n")
    }
}

fn emit_slot_fields(storage: SlotStorage) -> String {
    match storage {
        SlotStorage::Dense(len) if len <= INLINE_FRAME => inline_slot_fields(len),
        SlotStorage::Packed(_, len) => inline_slot_fields(len as usize),
        SlotStorage::Dense(_) => (
            "    bindings: Vec<Option<std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>>>,\n    capture_getters: Vec<Option<w3cos_core::Value>>,\n    capture_setters: Vec<Option<w3cos_core::Value>>,\n"
        )
            .into(),
        SlotStorage::Map => (
            "    bindings: std::collections::HashMap<\n        u32,\n        std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>,\n    >,\n    capture_getters: std::collections::HashMap<u32, w3cos_core::Value>,\n    capture_setters: std::collections::HashMap<u32, w3cos_core::Value>,\n"
        )
            .into(),
    }
}

fn inline_slot_fields(len: usize) -> String {
    format!(
        "    bindings: [Option<std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>>; {len}],\n    capture_getters: [Option<w3cos_core::Value>; {len}],\n    capture_setters: [Option<w3cos_core::Value>; {len}],\n"
    )
}

fn emit_bindings_new(storage: SlotStorage) -> String {
    match storage {
        SlotStorage::Dense(len) if len <= INLINE_FRAME => inline_bindings_new(len),
        SlotStorage::Packed(_, len) => inline_bindings_new(len as usize),
        SlotStorage::Dense(len) => format!("    let mut bindings = vec![None; {len}];\n"),
        SlotStorage::Map => "    let mut bindings = std::collections::HashMap::new();\n".into(),
    }
}

fn inline_bindings_new(len: usize) -> String {
    format!(
        "    let mut bindings: [Option<std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>>; {len}] = std::array::from_fn(|_| None);\n"
    )
}

fn emit_number_capture_slots(index: &str) -> String {
    format!(
        "w3cos_core::AotCapture::Number(cell) => {{\n                let __w3cos_number = std::rc::Rc::clone(cell);\n                let __w3cos_number_set = std::rc::Rc::clone(cell);\n                if let Some(slot) = capture_getters.get_mut({index}) {{ *slot = Some(w3cos_core::Value::function(move |_, _| match __w3cos_number.get() {{ Some(n) => w3cos_core::Value::Number(n), None => w3cos_core::Value::Undefined }})); }}\n                if let Some(slot) = capture_setters.get_mut({index}) {{ *slot = Some(w3cos_core::Value::function(move |_, arguments| {{ let next = arguments.first().map(|value| value.to_number()).unwrap_or(f64::NAN); __w3cos_number_set.set(Some(next)); w3cos_core::Value::Undefined }})); }}\n            }}"
    )
}

fn emit_capture_match_body(index: &str) -> String {
    let number = emit_number_capture_slots(index);
    format!(
        "match capture {{\n            w3cos_core::AotCapture::Accessors(getter, setter) => {{\n                if let Some(slot) = capture_getters.get_mut({index}) {{ *slot = Some(getter.clone()); }}\n                if let Some(slot) = capture_setters.get_mut({index}) {{ *slot = Some(setter.clone()); }}\n            }}\n            w3cos_core::AotCapture::Cell(cell) => {{\n                if let Some(slot) = bindings.get_mut({index}) {{ *slot = Some(std::rc::Rc::clone(cell)); }}\n            }}\n            {number}\n        }}"
    )
}

fn emit_capture_init(storage: SlotStorage) -> String {
    match storage {
        SlotStorage::Dense(len) if len <= INLINE_FRAME => {
            let body = emit_capture_match_body("index");
            format!(
                "    let mut capture_getters: [Option<w3cos_core::Value>; {len}] = std::array::from_fn(|_| None);\n    let mut capture_setters: [Option<w3cos_core::Value>; {len}] = std::array::from_fn(|_| None);\n    for (binding, capture) in __captures.iter() {{\n        let index = *binding as usize;\n        {body}\n    }}\n"
            )
        }
        SlotStorage::Packed(ids, len) => {
            let len = len as usize;
            let mut arms = String::new();
            for (index, id) in ids.iter().take(len).enumerate() {
                arms.push_str(&format!("{id} => {index}usize,\n"));
            }
            let body = emit_capture_match_body("index");
            format!(
                "    let mut capture_getters: [Option<w3cos_core::Value>; {len}] = std::array::from_fn(|_| None);\n    let mut capture_setters: [Option<w3cos_core::Value>; {len}] = std::array::from_fn(|_| None);\n    for (binding, capture) in __captures.iter() {{\n        let index = match *binding {{\n            {arms}            _ => continue,\n        }};\n        {body}\n    }}\n"
            )
        }
        SlotStorage::Dense(_) => {
            let number = emit_number_capture_slots("index");
            format!(
                "    let mut capture_getters = vec![None; bindings.len()];\n    let mut capture_setters = vec![None; bindings.len()];\n    for (binding, capture) in __captures.iter() {{\n        let index = *binding as usize;\n        match capture {{\n            w3cos_core::AotCapture::Accessors(getter, setter) => {{\n                if let Some(slot) = capture_getters.get_mut(index) {{ *slot = Some(getter.clone()); }}\n                if let Some(slot) = capture_setters.get_mut(index) {{ *slot = Some(setter.clone()); }}\n            }}\n            w3cos_core::AotCapture::Cell(cell) => {{\n                if let Some(slot) = bindings.get_mut(index) {{ *slot = Some(std::rc::Rc::clone(cell)); }}\n            }}\n            {number}\n        }}\n    }}\n"
            )
        }
        SlotStorage::Map => (
            "    let mut capture_getters = std::collections::HashMap::new();\n    let mut capture_setters = std::collections::HashMap::new();\n    for (binding, capture) in __captures.iter() {\n        match capture {\n            w3cos_core::AotCapture::Accessors(getter, setter) => {\n                capture_getters.insert(*binding, getter.clone());\n                capture_setters.insert(*binding, setter.clone());\n            }\n            w3cos_core::AotCapture::Cell(cell) => {\n                bindings.insert(*binding, std::rc::Rc::clone(cell));\n            }\n            w3cos_core::AotCapture::Number(cell) => {\n                let __w3cos_number = std::rc::Rc::clone(cell);\n                let __w3cos_number_set = std::rc::Rc::clone(cell);\n                capture_getters.insert(*binding, w3cos_core::Value::function(move |_, _| match __w3cos_number.get() { Some(n) => w3cos_core::Value::Number(n), None => w3cos_core::Value::Undefined }));\n                capture_setters.insert(*binding, w3cos_core::Value::function(move |_, arguments| { let next = arguments.first().map(|value| value.to_number()).unwrap_or(f64::NAN); __w3cos_number_set.set(Some(next)); w3cos_core::Value::Undefined }));\n            }\n        }\n    }\n"
        )
            .into(),
    }
}

fn emit_binding_assign(storage: SlotStorage, id: u32, value: &str) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!("        bindings[{index}] = Some({value});\n")
        }
        SlotStorage::Map => format!("        bindings.insert({id}, {value});\n"),
    }
}

fn emit_capture_get(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!("self.capture_getters.get({index}).and_then(|slot| slot.as_ref())")
        }
        SlotStorage::Map => format!("self.capture_getters.get(&{id})"),
    }
}

fn emit_capture_get_cloned(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!("self.capture_getters.get({index}).and_then(|slot| slot.clone())")
        }
        SlotStorage::Map => format!("self.capture_getters.get(&{id}).cloned()"),
    }
}

fn emit_capture_setter_get(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!("self.capture_setters.get({index}).and_then(|slot| slot.as_ref())")
        }
        SlotStorage::Map => format!("self.capture_setters.get(&{id})"),
    }
}

fn emit_capture_setter_get_cloned(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!("self.capture_setters.get({index}).and_then(|slot| slot.clone())")
        }
        SlotStorage::Map => format!("self.capture_setters.get(&{id}).cloned()"),
    }
}

fn emit_binding_get(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!("self.bindings.get({index}).and_then(|slot| slot.as_ref())")
        }
        SlotStorage::Map => format!("self.bindings.get(&{id})"),
    }
}

fn emit_binding_get_cloned(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!("self.bindings.get({index}).and_then(|slot| slot.clone())")
        }
        SlotStorage::Map => format!("self.bindings.get(&{id}).cloned()"),
    }
}

fn emit_binding_store_value(storage: SlotStorage, id: u32, rust_expr: &str) -> String {
    match storage {
        SlotStorage::Dense(_) | SlotStorage::Packed(_, _) => {
            let index = slot_index(storage, id);
            format!(
                "if let Some(slot) = self.bindings.get_mut({index}) {{ *slot = Some(std::rc::Rc::new(std::cell::RefCell::new({rust_expr}))); }}"
            )
        }
        SlotStorage::Map => format!(
            "self.bindings.insert({id}, std::rc::Rc::new(std::cell::RefCell::new({rust_expr})));"
        ),
    }
}

pub(crate) fn written_register_id(instruction: &Instruction) -> Option<u32> {
    written_register(instruction).map(|register| register.0)
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
        Instruction::InitializeBinding { .. }
        | Instruction::StoreBinding { .. }
        | Instruction::RefreshBinding { .. }
        | Instruction::SetProperty { .. }
        | Instruction::DefineField { .. }
        | Instruction::DefinePrivate { .. }
        | Instruction::SetPrivate { .. }
        | Instruction::DefinePrivateMethod { .. }
        | Instruction::DefinePrivateAccessor { .. }
        | Instruction::AppendArrayElement { .. }
        | Instruction::AppendIterable { .. }
        | Instruction::CopyDataProperties { .. }
        | Instruction::Jump { .. }
        | Instruction::Branch { .. }
        | Instruction::Return { .. }
        | Instruction::Throw { .. } => None,
    }
}

pub(crate) fn read_registers(instruction: &Instruction) -> Vec<u32> {
    match instruction {
        Instruction::LoadConstant { .. }
        | Instruction::LoadBinding { .. }
        | Instruction::RefreshBinding { .. }
        | Instruction::ImportMeta { .. }
        | Instruction::CreateClosure { .. }
        | Instruction::Jump { .. } => Vec::new(),
        Instruction::Move { src, .. } => vec![src.0],
        Instruction::InitializeBinding { value, .. } | Instruction::StoreBinding { value, .. } => {
            vec![value.0]
        }
        Instruction::Add { lhs, rhs, .. } | Instruction::Binary { lhs, rhs, .. } => {
            vec![lhs.0, rhs.0]
        }
        Instruction::Unary { value, .. } => vec![value.0],
        Instruction::GetProperty { object, key, .. }
        | Instruction::DeleteProperty { object, key, .. } => vec![object.0, key.0],
        Instruction::SetProperty {
            object, key, value, ..
        }
        | Instruction::DefineField {
            object, key, value, ..
        } => vec![object.0, key.0, value.0],
        Instruction::DefinePrivate {
            object,
            brand,
            name,
            value,
            ..
        } => vec![object.0, brand.0, name.0, value.0],
        Instruction::GetPrivate {
            object,
            brand,
            name,
            ..
        } => vec![object.0, brand.0, name.0],
        Instruction::SetPrivate {
            object,
            brand,
            name,
            value,
            ..
        } => vec![object.0, brand.0, name.0, value.0],
        Instruction::HasPrivate { object, brand, .. } => vec![object.0, brand.0],
        Instruction::DefinePrivateMethod {
            brand, name, value, ..
        } => {
            vec![brand.0, name.0, value.0]
        }
        Instruction::DefinePrivateAccessor {
            brand,
            name,
            getter,
            setter,
            ..
        } => {
            let mut registers = vec![brand.0, name.0];
            if let Some(getter) = getter {
                registers.push(getter.0);
            }
            if let Some(setter) = setter {
                registers.push(setter.0);
            }
            registers
        }
        Instruction::CreateArray { elements, .. } => {
            elements.iter().map(|element| element.0).collect()
        }
        Instruction::AppendArrayElement { array, value, .. } => vec![array.0, value.0],
        Instruction::AppendIterable {
            array, iterable, ..
        } => vec![array.0, iterable.0],
        Instruction::ArrayRest { value, .. } => vec![value.0],
        Instruction::ObjectRest {
            value, excluded, ..
        } => {
            let mut registers = vec![value.0];
            registers.extend(excluded.iter().map(|key| key.0));
            registers
        }
        Instruction::CreateObject { properties, .. } => properties
            .iter()
            .flat_map(|(key, value)| [key.0, value.0])
            .collect(),
        Instruction::CopyDataProperties { target, source, .. } => vec![target.0, source.0],
        Instruction::CreateClass {
            constructor,
            super_class,
            initializer,
            ..
        } => {
            let mut registers = vec![constructor.0];
            if let Some(super_class) = super_class {
                registers.push(super_class.0);
            }
            if let Some(initializer) = initializer {
                registers.push(initializer.0);
            }
            registers
        }
        Instruction::Call {
            callee,
            this_value,
            arguments,
            ..
        } => {
            let mut registers = vec![callee.0, this_value.0];
            registers.extend(arguments.iter().map(|argument| argument.0));
            registers
        }
        Instruction::CallWithArguments {
            callee,
            this_value,
            arguments,
            ..
        } => vec![callee.0, this_value.0, arguments.0],
        Instruction::CallMethod {
            object,
            key,
            arguments,
            ..
        } => {
            let mut registers = vec![object.0, key.0];
            registers.extend(arguments.iter().map(|argument| argument.0));
            registers
        }
        Instruction::CallMethodWithArguments {
            object,
            key,
            arguments,
            ..
        } => vec![object.0, key.0, arguments.0],
        Instruction::Construct {
            constructor,
            arguments,
            ..
        } => {
            let mut registers = vec![constructor.0];
            registers.extend(arguments.iter().map(|argument| argument.0));
            registers
        }
        Instruction::ConstructWithArguments {
            constructor,
            arguments,
            ..
        } => vec![constructor.0, arguments.0],
        Instruction::DynamicImport { specifier, .. } => vec![specifier.0],
        Instruction::Await { value, .. } | Instruction::Yield { value, .. } => vec![value.0],
        Instruction::YieldDelegate { iterator, .. } => vec![iterator.0],
        Instruction::Branch { condition, .. } => vec![condition.0],
        Instruction::Return { value } | Instruction::Throw { value } => vec![value.0],
    }
}

fn control_successors(instruction: &Instruction, function: &Function) -> Vec<BlockId> {
    match instruction {
        Instruction::Jump { target } => vec![*target],
        Instruction::Branch {
            then_block,
            else_block,
            ..
        } => {
            if then_block == else_block {
                vec![*then_block]
            } else {
                vec![*then_block, *else_block]
            }
        }
        Instruction::Await { suspension, .. } => function
            .suspension_points
            .iter()
            .find(|point| point.id == *suspension)
            .map(|point| vec![point.resume_block, point.reject_block])
            .unwrap_or_default(),
        Instruction::Yield { suspension, .. } => function
            .generator_suspension_points
            .iter()
            .find(|point| point.id == *suspension)
            .map(|point| vec![point.resume_block, point.return_block, point.throw_block])
            .unwrap_or_default(),
        Instruction::YieldDelegate { suspension, .. } => function
            .generator_suspension_points
            .iter()
            .find(|point| point.id == *suspension)
            .map(|point| vec![point.resume_block, point.return_block, point.throw_block])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn analyze_live_in(function: &Function) -> HashMap<BlockId, HashSet<u32>> {
    let mut live_in: HashMap<BlockId, HashSet<u32>> = HashMap::new();
    let mut changed = true;
    while changed {
        changed = false;
        for block in function.blocks.iter().rev() {
            let mut live = HashSet::new();
            if let Some(last) = block.instructions.last() {
                for successor in control_successors(last, function) {
                    if let Some(incoming) = live_in.get(&successor) {
                        live.extend(incoming.iter().copied());
                    }
                }
            }
            if let Some((_, catch)) = exception_target_for_block(function, block.id) {
                if let Some(incoming) = live_in.get(&catch) {
                    live.extend(incoming.iter().copied());
                }
            }
            for instruction in block.instructions.iter().rev() {
                if let Some(dst) = written_register(instruction) {
                    live.remove(&dst.0);
                }
                live.extend(read_registers(instruction));
            }
            let slot = live_in.entry(block.id).or_default();
            if slot != &live {
                *slot = live;
                changed = true;
            }
        }
    }
    live_in
}

fn last_uses_in_sequence(
    instructions: &[Instruction],
    function: &Function,
    exception_target: Option<(Register, BlockId)>,
    live_in: &HashMap<BlockId, HashSet<u32>>,
) -> Vec<HashSet<u32>> {
    let mut live = HashSet::new();
    if let Some(last) = instructions.last() {
        for successor in control_successors(last, function) {
            if let Some(incoming) = live_in.get(&successor) {
                live.extend(incoming.iter().copied());
            }
        }
    }
    if let Some((_, catch)) = exception_target {
        if let Some(incoming) = live_in.get(&catch) {
            live.extend(incoming.iter().copied());
        }
    }
    let mut last_uses = vec![HashSet::new(); instructions.len()];
    for (index, instruction) in instructions.iter().enumerate().rev() {
        let after = live.clone();
        if let Some(dst) = written_register(instruction) {
            live.remove(&dst.0);
        }
        let mut dying = HashSet::new();
        for register in read_registers(instruction) {
            if !after.contains(&register) {
                dying.insert(register);
            }
            live.insert(register);
        }
        last_uses[index] = dying;
    }
    last_uses
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

fn analyze_and_warn(function: &Function, specifier: Option<&str>) -> UniqueBackEdgePlan {
    let plan = analyze_unique_back_edges(function, specifier);
    for warning in &plan.warnings {
        eprintln!("warning: {warning}");
    }
    plan
}

/// Emit a native synchronous-generator factory for one W3IR function.
///
/// The initial backend covers the ordinary scalar/object instructions needed
/// by generator state-machine differential fixtures. Unsupported instructions
/// fail compilation explicitly; they are never approximated with divergent
/// behavior.
pub fn generate_generator(function: &Function, rust_name: &str) -> Result<String> {
    generate_generator_with_factories(function, rust_name, &HashMap::new(), None)
}

/// Emit a generator and every generator closure it creates from one validated
/// W3IR module. Nested factories share live binding cells through the capture
/// adapter ABI; ordinary AOT still links Core only.
pub fn generate_generator_from_module(
    module: &Module,
    function: &Function,
    rust_name: &str,
) -> Result<String> {
    let mut factories = HashMap::new();
    let mut emitted = HashSet::new();
    let mut visiting = HashSet::new();
    generate_function_tree(
        module,
        function,
        rust_name,
        &mut factories,
        &mut emitted,
        &mut visiting,
    )
}

fn generate_function_tree(
    module: &Module,
    function: &Function,
    rust_name: &str,
    factories: &mut HashMap<FunctionId, String>,
    emitted: &mut HashSet<FunctionId>,
    visiting: &mut HashSet<FunctionId>,
) -> Result<String> {
    if !visiting.insert(function.id) {
        bail!("recursive W3IR closure factory graph is not supported yet");
    }
    let install_constants = AOT_MODULE_DEPTH.with(|depth| {
        let outermost = depth.get() == 0;
        depth.set(depth.get() + 1);
        outermost
    });
    if install_constants {
        NUMERIC_CONSTANTS
            .with(|slot| *slot.borrow_mut() = module_numeric_constants(&module.functions));
        MODULE_FUNCTIONS.with(|slot| *slot.borrow_mut() = module.functions.clone());
        EVAL_BINDINGS.with(|cache| *cache.borrow_mut() = None);
    }
    let _constant_guard = NumericConstantGuard(install_constants);

    let mut output = String::new();
    for nested_id in function.blocks.iter().flat_map(|block| {
        block.instructions.iter().filter_map(|instruction| {
            if let Instruction::CreateClosure {
                function: nested, ..
            } = instruction
            {
                Some(*nested)
            } else {
                None
            }
        })
    }) {
        let nested = module
            .functions
            .iter()
            .find(|candidate| candidate.id == nested_id)
            .ok_or_else(|| anyhow!("missing nested W3IR function {nested_id:?}"))?;
        let nested_name = factories
            .entry(nested.id)
            .or_insert_with(|| {
                format!("{}__nested_{}", sanitize_identifier(rust_name), nested.id.0)
            })
            .clone();
        if !emitted.contains(&nested.id) {
            output.push_str(&generate_function_tree(
                module,
                nested,
                &nested_name,
                factories,
                emitted,
                visiting,
            )?);
        }
    }
    visiting.remove(&function.id);
    if function.is_generator {
        output.push_str(&generate_generator_with_factories(
            function,
            rust_name,
            factories,
            Some(&module.specifier),
        )?);
    } else if function.is_async {
        output.push_str(&generate_async_function_with_factories(
            function,
            rust_name,
            factories,
            Some(&module.specifier),
        )?);
    } else {
        output.push_str(&generate_sync_function_with_factories(
            function,
            rust_name,
            factories,
            Some(&module.specifier),
        )?);
    }
    emitted.insert(function.id);
    Ok(output)
}

/// Emit an ordinary async W3IR function as a native Core-only Promise state
/// machine. Await fulfillment and rejection resume the exact blocks recorded
/// in the validated W3IR suspension table.
pub fn generate_async_function_from_module(
    module: &Module,
    function: &Function,
    rust_name: &str,
) -> Result<String> {
    if function.is_generator || !function.is_async {
        bail!("async W3IR AOT emission requires a non-generator async function");
    }
    let mut factories = HashMap::new();
    let mut emitted = HashSet::new();
    let mut visiting = HashSet::new();
    generate_function_tree(
        module,
        function,
        rust_name,
        &mut factories,
        &mut emitted,
        &mut visiting,
    )
}

/// Emit a synchronous W3IR function whose closure instructions create
/// generator factories. This is the bridge needed for ordinary AOT functions
/// that return or invoke nested generators without linking W3VM.
pub fn generate_sync_function_from_module(
    module: &Module,
    function: &Function,
    rust_name: &str,
) -> Result<String> {
    if function.is_generator || function.is_async {
        bail!("sync W3IR AOT emission requires a non-async ordinary function");
    }
    let mut factories = HashMap::new();
    let mut emitted = HashSet::new();
    let mut visiting = HashSet::new();
    generate_function_tree(
        module,
        function,
        rust_name,
        &mut factories,
        &mut emitted,
        &mut visiting,
    )
}

fn generate_sync_function_with_factories(
    function: &Function,
    rust_name: &str,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
) -> Result<String> {
    let rust_name = sanitize_identifier(rust_name);
    let type_name = format!("{}Frame", upper_camel(&rust_name));
    let shared = shared_numeric_ids(function);
    SHARED_NUMBERS.with(|numbers| *numbers.borrow_mut() = shared.clone());
    let _shared_guard = SharedNumberGuard;
    let plan = analyze_function(function);
    let back_edges = analyze_and_warn(function, module_specifier);
    let (num_field, num_init) = emit_unboxed_reg_storage(&plan);
    let storage = slot_storage(function);
    let slot_fields = emit_slot_fields(storage);
    let slot_bindings_new = emit_bindings_new(storage);
    let slot_capture_init = emit_capture_init(storage);
    let sync_blocks = coalesce_sync_blocks(function);
    let mut blocks = String::new();
    let mut direct_body = None;
    for (block_id, instructions) in &sync_blocks {
        let exception_target = exception_target_for_block(function, *block_id);
        let native =
            find_native_loop(*block_id, instructions, &sync_blocks, function).and_then(|native| {
                emit_native_sync_loop(
                    instructions,
                    &sync_blocks,
                    &native,
                    function,
                    factories,
                    module_specifier,
                    &type_name,
                    storage,
                    &plan,
                    &back_edges,
                )
                .ok()
                .flatten()
            });
        let emitted = if let Some(native) = native {
            native
        } else {
            emit_block_instructions(
                instructions,
                exception_target,
                function,
                factories,
                EmissionMode::Sync,
                module_specifier,
                &type_name,
                storage,
                &plan,
                &back_edges,
            )?
        };
        if sync_blocks.len() == 1
            && *block_id == function.entry
            && exception_target.is_none()
            && instructions.last().is_some_and(|instruction| {
                matches!(
                    instruction,
                    Instruction::Return { .. } | Instruction::Throw { .. }
                )
            })
        {
            direct_body = Some(emitted.clone());
        }
        blocks.push_str(&format!("                {} => {{\n", block_id.0));
        blocks.push_str(&emitted);
        blocks.push_str("                }\n");
    }
    let is_direct = direct_body.is_some();
    let run_body = if let Some(direct_body) = &direct_body {
        direct_body.clone()
    } else {
        format!(
            r#"
        'drive: loop {{
            match self.block {{
{blocks}                _ => return Err(
                    w3cos_core::Value::string("invalid synchronous W3IR block")
                ),
            }}
        }}"#
        )
    };
    let block_field = if is_direct { "" } else { "    block: u32,\n" };
    let block_initializer = if is_direct {
        String::new()
    } else {
        format!("        block: {},\n", function.entry.0)
    };

    let mut binding_initializers = String::new();
    let skip_unread_this = function
        .this_binding
        .is_some_and(|binding| !binding_is_used(function, binding.0));
    for binding in &function.bindings {
        if num_binding_slot(&plan, binding.id.0).is_some() || shared.contains(&binding.id.0) {
            continue;
        }
        if skip_unread_this && function.this_binding == Some(binding.id) {
            continue;
        }
        let initialized = matches!(
            binding.kind,
            BindingKind::Var | BindingKind::Function | BindingKind::Parameter
        );
        binding_initializers.push_str(&emit_binding_assign(
            storage,
            binding.id.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized})))"
            ),
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true)))"
            ),
        ));
    }
    if let Some(binding) = function.arguments_binding {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.to_vec()), true)))",
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true)))",
                function.parameters.len()
            ),
        ));
    }
    if let Some(binding) = function.this_binding
        && !skip_unread_this
    {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((__this, true)))",
        ));
    }
    let (num_cells_field, num_cells_local, num_cells_init) = emit_num_cells(&shared);
    let fast_prefix = emit_number_bump_prefix(function);

    Ok(format!(
        r#"
struct {type_name} {{
{registers_field}
{num_field}{slot_fields}{num_cells_field}{block_field}
}}

impl {type_name} {{
    fn run(&mut self) -> Result<w3cos_core::Value, w3cos_core::Value> {{
{run_body}
    }}
}}

pub fn {rust_name}(
    __this: w3cos_core::Value,
    __args: &[w3cos_core::Value],
    __captures: &w3cos_core::AotCaptureMap,
) -> w3cos_core::Value {{
{fast_prefix}{num_cells_local}{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    {type_name} {{
{registers_init}
{num_init}        bindings,
        capture_getters,
        capture_setters,
{num_cells_init}{block_initializer}
    }}.run().unwrap_or_else(|__w3cos_exception| w3cos_core::throw_value(__w3cos_exception))
}}
"#,
        registers_field = emit_registers_field(plan.n_val),
        registers_init = emit_registers_init(plan.n_val),
    ))
}

fn generate_async_function_with_factories(
    function: &Function,
    rust_name: &str,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
) -> Result<String> {
    if function.is_generator || !function.is_async {
        bail!("ordinary async W3IR AOT emission requires an async non-generator function");
    }
    let rust_name = sanitize_identifier(rust_name);
    let type_name = format!("{}Frame", upper_camel(&rust_name));
    let plan = analyze_function(function);
    let back_edges = analyze_and_warn(function, module_specifier);
    let (num_field, num_init) = emit_unboxed_reg_storage(&plan);
    let storage = slot_storage(function);
    let slot_fields = emit_slot_fields(storage);
    let slot_bindings_new = emit_bindings_new(storage);
    let slot_capture_init = emit_capture_init(storage);
    let mut blocks = String::new();
    for block in &function.blocks {
        let exception_target = function
            .exception_regions
            .iter()
            .find(|region| region.protected_blocks.contains(&block.id))
            .and_then(|region| {
                region
                    .catch_block
                    .or(region.finally_block)
                    .map(|target| (region.exception, target))
            });
        blocks.push_str(&format!("                {} => {{\n", block.id.0));
        blocks.push_str(&emit_block_instructions(
            &block.instructions,
            exception_target,
            function,
            factories,
            EmissionMode::Async,
            module_specifier,
            &type_name,
            storage,
            &plan,
            &back_edges,
        )?);
        blocks.push_str("                }\n");
    }

    let mut binding_initializers = String::new();
    for binding in &function.bindings {
        if num_binding_slot(&plan, binding.id.0).is_some() {
            continue;
        }
        let initialized = matches!(
            binding.kind,
            BindingKind::Var | BindingKind::Function | BindingKind::Parameter
        );
        binding_initializers.push_str(&emit_binding_assign(
            storage,
            binding.id.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized})))"
            ),
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true)))"
            ),
        ));
    }
    if let Some(binding) = function.arguments_binding {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.to_vec()), true)))",
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true)))",
                function.parameters.len()
            ),
        ));
    }
    if let Some(binding) = function.this_binding {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((__this, true)))",
        ));
    }

    Ok(format!(
        r#"
enum {type_name}Outcome {{
    Await {{
        value: w3cos_core::Value,
        dst: u32,
        resume: u32,
        reject: u32,
    }},
    Complete(w3cos_core::Value),
}}

struct {type_name} {{
{registers_field}
{num_field}{slot_fields}    block: u32,
}}

impl {type_name} {{
    fn completed(value: w3cos_core::Value) -> {type_name}Outcome {{
        {type_name}Outcome::Complete(value)
    }}

    fn awaited(
        value: w3cos_core::Value,
        dst: u32,
        resume_block: u32,
        reject_block: u32,
    ) -> {type_name}Outcome {{
        {type_name}Outcome::Await {{
            value,
            dst,
            resume: resume_block,
            reject: reject_block,
        }}
    }}

    fn run(&mut self) -> Result<{type_name}Outcome, w3cos_core::Value> {{
        'drive: loop {{
            match self.block {{
{blocks}                _ => return Err(
                    w3cos_core::Value::string("invalid async W3IR block")
                ),
            }}
        }}
    }}

    fn drive(
        frame: std::rc::Rc<std::cell::RefCell<Self>>,
        resolve: w3cos_core::Value,
        reject: w3cos_core::Value,
    ) {{
        let outcome = match w3cos_core::catch_js_result(|| frame.borrow_mut().run()) {{
            Ok(outcome) => outcome,
            Err(reason) => {{
                reject.call(w3cos_core::Value::Undefined, vec![reason]);
                return;
            }}
        }};
        match outcome {{
            {type_name}Outcome::Complete(value) => {{
                resolve.call(w3cos_core::Value::Undefined, vec![value]);
            }}
            {type_name}Outcome::Await {{
                value,
                dst,
                resume,
                reject: reject_block,
            }} => {{
                let dst = dst as usize;
                let awaited = w3cos_core::intrinsics::await_value(&value);
                if let Some(w3cos_core::promise::PromiseStatus::Fulfilled(ready)) =
                    w3cos_core::promise::status(&awaited)
                {{
                    {{
                        let mut frame = frame.borrow_mut();
                        frame.registers[dst] = ready;
                        frame.block = resume;
                    }}
                    Self::drive(frame, resolve, reject);
                    return;
                }}
                let fulfilled_frame = std::rc::Rc::clone(&frame);
                let fulfilled_resolve = resolve.clone();
                let fulfilled_reject = reject.clone();
                let on_fulfilled = w3cos_core::Value::function(move |_, arguments| {{
                    {{
                        let mut frame = fulfilled_frame.borrow_mut();
                        frame.registers[dst] =
                            arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                        frame.block = resume;
                    }}
                    Self::drive(
                        std::rc::Rc::clone(&fulfilled_frame),
                        fulfilled_resolve.clone(),
                        fulfilled_reject.clone(),
                    );
                    w3cos_core::Value::Undefined
                }});
                let rejected_frame = std::rc::Rc::clone(&frame);
                let rejected_resolve = resolve.clone();
                let rejected_reject = reject.clone();
                let on_rejected = w3cos_core::Value::function(move |_, arguments| {{
                    {{
                        let mut frame = rejected_frame.borrow_mut();
                        frame.registers[dst] =
                            arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                        frame.block = reject_block;
                    }}
                    Self::drive(
                        std::rc::Rc::clone(&rejected_frame),
                        rejected_resolve.clone(),
                        rejected_reject.clone(),
                    );
                    w3cos_core::Value::Undefined
                }});
                w3cos_core::intrinsics::call_method(
                    &awaited,
                    &w3cos_core::Value::string("then"),
                    vec![on_fulfilled, on_rejected],
                );
            }}
        }}
    }}
}}

pub fn {rust_name}(
    __this: w3cos_core::Value,
    __args: &[w3cos_core::Value],
    __captures: &w3cos_core::AotCaptureMap,
) -> w3cos_core::Value {{
{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
{registers_init}
{num_init}        bindings,
        capture_getters,
        capture_setters,
        block: {entry},
    }}));
    w3cos_core::intrinsics::promise_new(vec![w3cos_core::Value::function(move |_, arguments| {{
        let resolve = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
        let reject = arguments.get(1).cloned().unwrap_or(w3cos_core::Value::Undefined);
        {type_name}::drive(std::rc::Rc::clone(&frame), resolve, reject);
        w3cos_core::Value::Undefined
    }})])
}}
"#,
        registers_field = emit_registers_field(plan.n_val),
        registers_init = emit_registers_init(plan.n_val),
        entry = function.entry.0,
    ))
}

fn generate_generator_with_factories(
    function: &Function,
    rust_name: &str,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
) -> Result<String> {
    if !function.is_generator {
        bail!("W3IR function {:?} is not a generator", function.id);
    }
    if function.is_async {
        return generate_async_generator_with_factories(
            function,
            rust_name,
            factories,
            module_specifier,
        );
    }
    let rust_name = sanitize_identifier(rust_name);
    let type_name = format!("{}Frame", upper_camel(&rust_name));
    let plan = analyze_function(function);
    let back_edges = analyze_and_warn(function, module_specifier);
    let (num_field, num_init) = emit_unboxed_reg_storage(&plan);
    let storage = slot_storage(function);
    let slot_fields = emit_slot_fields(storage);
    let slot_bindings_new = emit_bindings_new(storage);
    let slot_capture_init = emit_capture_init(storage);

    let mut blocks = String::new();
    for block in &function.blocks {
        let exception_target = function
            .exception_regions
            .iter()
            .find(|region| region.protected_blocks.contains(&block.id))
            .and_then(|region| {
                region
                    .catch_block
                    .or(region.finally_block)
                    .map(|target| (region.exception, target))
            });
        blocks.push_str(&format!("                {} => {{\n", block.id.0));
        blocks.push_str(&emit_block_instructions(
            &block.instructions,
            exception_target,
            function,
            factories,
            EmissionMode::Generator,
            module_specifier,
            &type_name,
            storage,
            &plan,
            &back_edges,
        )?);
        blocks.push_str("                }\n");
    }

    let mut suspension_arms = String::new();
    let mut delegate_suspension_arms = String::new();
    for point in &function.generator_suspension_points {
        suspension_arms.push_str(&format!(
            "                    {} => {{ {}; self.block = match kind {{ 0 => {}, 1 => {}, _ => {} }}; }}\n",
            point.id.0,
            store_boxed(&plan, point.result.0, "input"),
            point.resume_block.0,
            point.return_block.0,
            point.throw_block.0,
        ));
        delegate_suspension_arms.push_str(&format!(
            r#"                    {suspension} => {{
                        let iterator = self.delegate_iterator.clone().unwrap_or_else(|| w3cos_core::throw_value(w3cos_core::Value::string("missing delegated iterator")));
                        let method = match kind {{ 0 => "next", 1 => "return", _ => "throw" }};
                        let mut delegated = Self::delegate_step(&iterator, method, vec![input.clone()]);
                        if kind == 2 && delegated.is_none() {{
                            let _ = Self::delegate_step(&iterator, "return", Vec::new());
                            self.registers[{result}] = w3cos_core::Value::string("TypeError: delegated iterator has no throw method");
                            self.delegate_iterator = None;
                            self.block = {throw_block};
                        }} else if kind == 1 && delegated.is_none() {{
                            self.registers[{result}] = input;
                            self.delegate_iterator = None;
                            self.block = {return_block};
                        }} else {{
                            let delegated = delegated.take().unwrap_or_else(|| w3cos_core::throw_value(w3cos_core::Value::string("TypeError: delegated iterator method is missing")));
                            let value = delegated.get_property("value");
                            if delegated.get_property("done").to_bool() {{
                                self.registers[{result}] = value;
                                self.delegate_iterator = None;
                                self.block = if kind == 1 {{ {return_block} }} else {{ {resume_block} }};
                            }} else {{
                                self.state = {type_name}State::SuspendedDelegate({suspension});
                                return Self::result(value, false);
                            }}
                        }}
                    }}
"#,
            suspension = point.id.0,
            result = val_index(&plan, point.result.0),
            throw_block = point.throw_block.0,
            return_block = point.return_block.0,
            resume_block = point.resume_block.0,
        ));
    }

    let mut binding_initializers = String::new();
    for binding in &function.bindings {
        if num_binding_slot(&plan, binding.id.0).is_some() {
            continue;
        }
        let initialized = matches!(
            binding.kind,
            BindingKind::Var | BindingKind::Function | BindingKind::Parameter
        );
        binding_initializers.push_str(&emit_binding_assign(
            storage,
            binding.id.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized})))"
            ),
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true)))"
            ),
        ));
    }
    if let Some(binding) = function.arguments_binding {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.to_vec()), true)))",
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true)))",
                function.parameters.len()
            ),
        ));
    }
    if let Some(binding) = function.this_binding {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((__this, true)))",
        ));
    }

    Ok(format!(
        r#"
#[derive(Clone, Copy)]
enum {type_name}State {{
    Start,
    Executing,
    Suspended(u32),
    SuspendedDelegate(u32),
    Completed,
}}

struct {type_name} {{
    state: {type_name}State,
{registers_field}
{num_field}{slot_fields}    delegate_iterator: Option<w3cos_core::Value>,
    block: u32,
}}

impl {type_name} {{
    fn result(value: w3cos_core::Value, done: bool) -> w3cos_core::Value {{
        w3cos_core::Value::object(std::collections::HashMap::from([
            ("value".to_string(), value),
            ("done".to_string(), w3cos_core::Value::Bool(done)),
        ]))
    }}

    fn delegate_step(
        iterator: &w3cos_core::Value,
        method: &str,
        arguments: Vec<w3cos_core::Value>,
    ) -> Option<w3cos_core::Value> {{
        let method = iterator.get_property(method);
        if method.is_nullish() {{
            return None;
        }}
        if !method.is_callable() {{
            return w3cos_core::throw_value(
                w3cos_core::Value::string("TypeError: delegated iterator method is not callable")
            );
        }}
        let result = method.call(iterator.clone(), arguments);
        if !result.is_object() && !result.is_function() {{
            return w3cos_core::throw_value(
                w3cos_core::Value::string("TypeError: delegated iterator result is not an object")
            );
        }}
        Some(result)
    }}

    fn resume(&mut self, kind: u8, input: w3cos_core::Value) -> w3cos_core::Value {{
        match self.state {{
            {type_name}State::Executing => w3cos_core::throw_value(
                w3cos_core::Value::string("TypeError: generator is already executing")
            ),
            {type_name}State::Completed => return match kind {{
                0 => Self::result(w3cos_core::Value::Undefined, true),
                1 => Self::result(input, true),
                _ => w3cos_core::throw_value(input),
            }},
            {type_name}State::Start => match kind {{
                0 => {{}},
                1 => {{
                    self.state = {type_name}State::Completed;
                    return Self::result(input, true);
                }}
                _ => {{
                    self.state = {type_name}State::Completed;
                    return w3cos_core::throw_value(input);
                }}
            }},
            {type_name}State::Suspended(suspension) => match suspension {{
{suspension_arms}                _ => w3cos_core::throw_value(
                    w3cos_core::Value::string("invalid generator suspension")
                ),
            }},
            {type_name}State::SuspendedDelegate(suspension) => match suspension {{
{delegate_suspension_arms}                _ => w3cos_core::throw_value(
                    w3cos_core::Value::string("invalid generator delegation")
                ),
            }},
        }}
        self.state = {type_name}State::Executing;
        'drive: loop {{
            match self.block {{
{blocks}                _ => {{
                    self.state = {type_name}State::Completed;
                    return w3cos_core::throw_value(
                        w3cos_core::Value::string("invalid generator block")
                    );
                }}
            }}
        }}
    }}
}}

pub fn {rust_name}(
    __this: w3cos_core::Value,
    __args: &[w3cos_core::Value],
    __captures: &w3cos_core::AotCaptureMap,
) -> w3cos_core::Value {{
{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        state: {type_name}State::Start,
{registers_init}
{num_init}        bindings,
        capture_getters,
        capture_setters,
        delegate_iterator: None,
        block: {entry},
    }}));
    let generator = w3cos_core::Value::object(std::collections::HashMap::new());
    for (name, kind) in [("next", 0_u8), ("return", 1_u8), ("throw", 2_u8)] {{
        let frame = std::rc::Rc::clone(&frame);
        generator.set_property(name, w3cos_core::Value::function(move |_, arguments| {{
            let input = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
            frame.borrow_mut().resume(kind, input)
        }}));
    }}
    // `[Symbol.iterator]()` returns its receiver; capturing `generator`
    // here would form an uncollectable generator -> function -> generator cycle.
    generator.set_property(
        "__w3cos_symbol_iterator",
        w3cos_core::Value::function(|this, _| this),
    );
    generator
}}
"#,
        registers_field = emit_registers_field(plan.n_val),
        registers_init = emit_registers_init(plan.n_val),
        entry = function.entry.0,
    ))
}

fn generate_async_generator_with_factories(
    function: &Function,
    rust_name: &str,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
) -> Result<String> {
    let rust_name = sanitize_identifier(rust_name);
    let type_name = format!("{}Frame", upper_camel(&rust_name));
    let plan = analyze_function(function);
    let back_edges = analyze_and_warn(function, module_specifier);
    let (num_field, num_init) = emit_unboxed_reg_storage(&plan);
    let storage = slot_storage(function);
    let slot_fields = emit_slot_fields(storage);
    let slot_bindings_new = emit_bindings_new(storage);
    let slot_capture_init = emit_capture_init(storage);
    let request_name = format!("{type_name}Request");

    let mut blocks = String::new();
    for block in &function.blocks {
        let exception_target = function
            .exception_regions
            .iter()
            .find(|region| region.protected_blocks.contains(&block.id))
            .and_then(|region| {
                region
                    .catch_block
                    .or(region.finally_block)
                    .map(|target| (region.exception, target))
            });
        blocks.push_str(&format!("                {} => {{\n", block.id.0));
        blocks.push_str(&emit_block_instructions(
            &block.instructions,
            exception_target,
            function,
            factories,
            EmissionMode::AsyncGenerator,
            module_specifier,
            &type_name,
            storage,
            &plan,
            &back_edges,
        )?);
        blocks.push_str("                }\n");
    }

    let mut suspension_arms = String::new();
    let mut rejected_yield_arms = String::new();
    let mut delegate_resume_arms = String::new();
    let mut delegate_result_arms = String::new();
    for point in &function.generator_suspension_points {
        suspension_arms.push_str(&format!(
            "                    {} => {{ {}; self.block = match kind {{ 0 => {}, 1 => {}, _ => {} }}; }}\n",
            point.id.0,
            store_boxed(&plan, point.result.0, "input"),
            point.resume_block.0,
            point.return_block.0,
            point.throw_block.0,
        ));
        rejected_yield_arms.push_str(&format!(
            "            {} => {{ {}; self.block = {}; self.state = {type_name}State::Ready; }}\n",
            point.id.0,
            store_boxed(&plan, point.result.0, "reason"),
            point.throw_block.0
        ));
        delegate_resume_arms.push_str(&format!(
            r#"                    {suspension} => {{
                        let iterator = self.delegate_iterator.clone().unwrap_or_else(|| w3cos_core::throw_value(w3cos_core::Value::string("missing delegated iterator")));
                        let method = match kind {{ 0 => "next", 1 => "return", _ => "throw" }};
                        match Self::delegate_call(&iterator, method, vec![input.clone()]) {{
                            Ok(Some(awaited)) => return Self::delegated(awaited, {suspension}, kind),
                            Err(reason) => {{
                                self.registers[{result}] = reason;
                                self.delegate_iterator = None;
                                self.block = {throw_block};
                            }}
                            Ok(None) if kind == 1 => {{
                                self.registers[{result}] = input;
                                self.delegate_iterator = None;
                                self.block = {return_block};
                            }}
                            Ok(None) if kind == 2 => {{
                                match Self::delegate_call(&iterator, "return", Vec::new()) {{
                                    Ok(Some(awaited)) => return Self::delegated(awaited, {suspension}, 3),
                                    Err(reason) => {{
                                        self.registers[{result}] = reason;
                                        self.delegate_iterator = None;
                                        self.block = {throw_block};
                                    }}
                                    Ok(None) => {{
                                        self.registers[{result}] = w3cos_core::Value::string("TypeError: delegated iterator has no throw method");
                                        self.delegate_iterator = None;
                                        self.block = {throw_block};
                                    }}
                                }}
                            }}
                            Ok(None) => {{
                                self.registers[{result}] = w3cos_core::Value::string("TypeError: delegated iterator has no next method");
                                self.delegate_iterator = None;
                                self.block = {throw_block};
                            }}
                        }}
                    }}
"#,
            suspension = point.id.0,
            result = val_index(&plan, point.result.0),
            return_block = point.return_block.0,
            throw_block = point.throw_block.0,
        ));
        delegate_result_arms.push_str(&format!(
            r#"            {suspension} => {{
                if action == 3 {{
                    self.registers[{result}] = w3cos_core::Value::string("TypeError: delegated iterator has no throw method");
                    self.delegate_iterator = None;
                    self.block = {throw_block};
                }} else {{
                    self.registers[{result}] = value;
                    self.delegate_iterator = None;
                    self.block = if action == 1 {{ {return_block} }} else {{ {resume_block} }};
                }}
                self.state = {type_name}State::Ready;
            }}
"#,
            suspension = point.id.0,
            result = val_index(&plan, point.result.0),
            throw_block = point.throw_block.0,
            return_block = point.return_block.0,
            resume_block = point.resume_block.0,
        ));
    }

    let mut binding_initializers = String::new();
    for binding in &function.bindings {
        if num_binding_slot(&plan, binding.id.0).is_some() {
            continue;
        }
        let initialized = matches!(
            binding.kind,
            BindingKind::Var | BindingKind::Function | BindingKind::Parameter
        );
        binding_initializers.push_str(&emit_binding_assign(
            storage,
            binding.id.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized})))"
            ),
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true)))"
            ),
        ));
    }
    if let Some(binding) = function.arguments_binding {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.to_vec()), true)))",
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            &format!(
                "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true)))",
                function.parameters.len()
            ),
        ));
    }
    if let Some(binding) = function.this_binding {
        parameter_initializers.push_str(&emit_binding_assign(
            storage,
            binding.0,
            "std::rc::Rc::new(std::cell::RefCell::new((__this, true)))",
        ));
    }

    Ok(format!(
        r#"
#[derive(Clone, Copy)]
enum {type_name}State {{
    Start,
    Ready,
    Executing,
    Suspended(u32),
    SuspendedDelegate(u32),
    Completed,
}}

struct {request_name} {{
    kind: u8,
    input: w3cos_core::Value,
    resolve: w3cos_core::Value,
    reject: w3cos_core::Value,
}}

struct {type_name} {{
    state: {type_name}State,
{registers_field}
{num_field}{slot_fields}    delegate_iterator: Option<w3cos_core::Value>,
    block: u32,
    queue: std::collections::VecDeque<{request_name}>,
    active: Option<{request_name}>,
}}

impl {type_name} {{
    fn result(value: w3cos_core::Value, done: bool) -> w3cos_core::Value {{
        w3cos_core::Value::object(std::collections::HashMap::from([
            ("value".to_string(), value),
            ("done".to_string(), w3cos_core::Value::Bool(done)),
        ]))
    }}

    fn awaited(
        value: w3cos_core::Value,
        dst: u32,
        resume_block: u32,
        reject_block: u32,
    ) -> w3cos_core::Value {{
        w3cos_core::Value::object(std::collections::HashMap::from([
            ("__w3cos_async_generator_await".to_string(), w3cos_core::Value::Bool(true)),
            ("value".to_string(), value),
            ("dst".to_string(), w3cos_core::Value::Number(dst as f64)),
            ("resume".to_string(), w3cos_core::Value::Number(resume_block as f64)),
            ("reject".to_string(), w3cos_core::Value::Number(reject_block as f64)),
        ]))
    }}

    fn delegated(
        value: w3cos_core::Value,
        suspension: u32,
        action: u8,
    ) -> w3cos_core::Value {{
        w3cos_core::Value::object(std::collections::HashMap::from([
            ("__w3cos_async_generator_delegate".to_string(), w3cos_core::Value::Bool(true)),
            ("value".to_string(), value),
            ("suspension".to_string(), w3cos_core::Value::Number(suspension as f64)),
            ("action".to_string(), w3cos_core::Value::Number(action as f64)),
        ]))
    }}

    fn delegate_call(
        iterator: &w3cos_core::Value,
        method: &str,
        arguments: Vec<w3cos_core::Value>,
    ) -> Result<Option<w3cos_core::Value>, w3cos_core::Value> {{
        let method = iterator.get_property(method);
        if method.is_nullish() {{
            return Ok(None);
        }}
        if !method.is_callable() {{
            return Err(w3cos_core::Value::string(
                "TypeError: delegated iterator method is not callable"
            ));
        }}
        match w3cos_core::catch_js(|| method.call(iterator.clone(), arguments)) {{
            Ok(value) => Ok(Some(value)),
            Err(value) => Err(value),
        }}
    }}

    fn resume(&mut self, kind: u8, input: w3cos_core::Value) -> w3cos_core::Value {{
        match self.state {{
            {type_name}State::Executing => w3cos_core::throw_value(
                w3cos_core::Value::string("TypeError: generator is already executing")
            ),
            {type_name}State::Completed => return match kind {{
                0 => Self::result(w3cos_core::Value::Undefined, true),
                1 => Self::result(input, true),
                _ => w3cos_core::throw_value(input),
            }},
            {type_name}State::Start => match kind {{
                0 => {{}},
                1 => {{
                    self.state = {type_name}State::Completed;
                    return Self::result(input, true);
                }}
                _ => {{
                    self.state = {type_name}State::Completed;
                    return w3cos_core::throw_value(input);
                }}
            }},
            {type_name}State::Ready => {{}},
            {type_name}State::Suspended(suspension) => match suspension {{
{suspension_arms}                _ => w3cos_core::throw_value(
                    w3cos_core::Value::string("invalid async-generator suspension")
                ),
            }},
            {type_name}State::SuspendedDelegate(suspension) => match suspension {{
{delegate_resume_arms}                _ => w3cos_core::throw_value(
                    w3cos_core::Value::string("invalid async-generator delegation")
                ),
            }},
        }}
        self.state = {type_name}State::Executing;
        'drive: loop {{
            match self.block {{
{blocks}                _ => {{
                    self.state = {type_name}State::Completed;
                    return w3cos_core::throw_value(
                        w3cos_core::Value::string("invalid async-generator block")
                    );
                }}
            }}
        }}
    }}

    fn reject_yield(&mut self, suspension: u32, reason: w3cos_core::Value) {{
        match suspension {{
{rejected_yield_arms}            _ => {{
                self.state = {type_name}State::Completed;
            }}
        }}
    }}

    fn complete_delegate(
        &mut self,
        suspension: u32,
        action: u8,
        value: w3cos_core::Value,
    ) {{
        match suspension {{
{delegate_result_arms}            _ => {{
                self.state = {type_name}State::Completed;
            }}
        }}
    }}

    fn settle_active(
        frame: std::rc::Rc<std::cell::RefCell<Self>>,
        fulfilled: bool,
        value: w3cos_core::Value,
    ) {{
        let request = frame.borrow_mut().active.take();
        if let Some(request) = request {{
            let callback = if fulfilled {{ request.resolve }} else {{ request.reject }};
            callback.call(w3cos_core::Value::Undefined, vec![value]);
        }}
        Self::drive(frame);
    }}

    fn handle(
        frame: std::rc::Rc<std::cell::RefCell<Self>>,
        outcome: w3cos_core::Value,
    ) {{
        if outcome
            .get_property("__w3cos_async_generator_delegate")
            .to_bool()
        {{
            let suspension = outcome.get_property("suspension").to_number() as u32;
            let action = outcome.get_property("action").to_number() as u8;
            let fulfilled_frame = std::rc::Rc::clone(&frame);
            let on_fulfilled = w3cos_core::Value::function(move |_, arguments| {{
                let result = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                if !result.is_object() && !result.is_function() {{
                    fulfilled_frame.borrow_mut().reject_yield(
                        suspension,
                        w3cos_core::Value::string(
                            "TypeError: delegated iterator result is not an object"
                        ),
                    );
                    Self::continue_active(std::rc::Rc::clone(&fulfilled_frame));
                    return w3cos_core::Value::Undefined;
                }}
                if action == 3 {{
                    fulfilled_frame.borrow_mut().complete_delegate(
                        suspension,
                        action,
                        w3cos_core::Value::Undefined,
                    );
                    Self::continue_active(std::rc::Rc::clone(&fulfilled_frame));
                    return w3cos_core::Value::Undefined;
                }}
                let value = result.get_property("value");
                if result.get_property("done").to_bool() {{
                    fulfilled_frame
                        .borrow_mut()
                        .complete_delegate(suspension, action, value);
                    Self::continue_active(std::rc::Rc::clone(&fulfilled_frame));
                }} else {{
                    let yielded_frame = std::rc::Rc::clone(&fulfilled_frame);
                    let on_yielded = w3cos_core::Value::function(move |_, arguments| {{
                        let value = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                        yielded_frame.borrow_mut().state =
                            {type_name}State::SuspendedDelegate(suspension);
                        Self::settle_active(
                            std::rc::Rc::clone(&yielded_frame),
                            true,
                            Self::result(value, false),
                        );
                        w3cos_core::Value::Undefined
                    }});
                    let rejected_yield_frame = std::rc::Rc::clone(&fulfilled_frame);
                    let on_yield_rejected = w3cos_core::Value::function(move |_, arguments| {{
                        let reason = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                        rejected_yield_frame
                            .borrow_mut()
                            .reject_yield(suspension, reason);
                        Self::continue_active(std::rc::Rc::clone(&rejected_yield_frame));
                        w3cos_core::Value::Undefined
                    }});
                    let awaited = w3cos_core::intrinsics::await_value(&value);
                    w3cos_core::intrinsics::call_method(
                        &awaited,
                        &w3cos_core::Value::string("then"),
                        vec![on_yielded, on_yield_rejected],
                    );
                }}
                w3cos_core::Value::Undefined
            }});
            let rejected_frame = std::rc::Rc::clone(&frame);
            let on_rejected = w3cos_core::Value::function(move |_, arguments| {{
                let reason = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                rejected_frame.borrow_mut().reject_yield(suspension, reason);
                Self::continue_active(std::rc::Rc::clone(&rejected_frame));
                w3cos_core::Value::Undefined
            }});
            let awaited = w3cos_core::intrinsics::await_value(
                &w3cos_core::intrinsics::get_property(
                    &outcome,
                    &w3cos_core::Value::string("value"),
                ),
            );
            w3cos_core::intrinsics::call_method(
                &awaited,
                &w3cos_core::Value::string("then"),
                vec![on_fulfilled, on_rejected],
            );
            return;
        }}

        if outcome
            .get_property("__w3cos_async_generator_await")
            .to_bool()
        {{
            let dst = outcome.get_property("dst").to_number() as usize;
            let resume_block = outcome.get_property("resume").to_number() as u32;
            let reject_block = outcome.get_property("reject").to_number() as u32;
            let fulfilled_frame = std::rc::Rc::clone(&frame);
            let on_fulfilled = w3cos_core::Value::function(move |_, arguments| {{
                let value = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                {{
                    let mut frame = fulfilled_frame.borrow_mut();
                    frame.registers[dst] = value;
                    frame.block = resume_block;
                    frame.state = {type_name}State::Ready;
                }}
                Self::continue_active(std::rc::Rc::clone(&fulfilled_frame));
                w3cos_core::Value::Undefined
            }});
            let rejected_frame = std::rc::Rc::clone(&frame);
            let on_rejected = w3cos_core::Value::function(move |_, arguments| {{
                let reason = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                {{
                    let mut frame = rejected_frame.borrow_mut();
                    frame.registers[dst] = reason;
                    frame.block = reject_block;
                    frame.state = {type_name}State::Ready;
                }}
                Self::continue_active(std::rc::Rc::clone(&rejected_frame));
                w3cos_core::Value::Undefined
            }});
            let awaited = w3cos_core::intrinsics::await_value(
                &w3cos_core::intrinsics::get_property(
                    &outcome,
                    &w3cos_core::Value::string("value"),
                ),
            );
            w3cos_core::intrinsics::call_method(
                &awaited,
                &w3cos_core::Value::string("then"),
                vec![on_fulfilled, on_rejected],
            );
            return;
        }}

        let done = outcome.get_property("done").to_bool();
        let value = outcome.get_property("value");
        let fulfilled_frame = std::rc::Rc::clone(&frame);
        let on_fulfilled = w3cos_core::Value::function(move |_, arguments| {{
            let value = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
            Self::settle_active(
                std::rc::Rc::clone(&fulfilled_frame),
                true,
                Self::result(value, done),
            );
            w3cos_core::Value::Undefined
        }});
        let rejected_frame = std::rc::Rc::clone(&frame);
        let on_rejected = w3cos_core::Value::function(move |_, arguments| {{
            let reason = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
            if done {{
                rejected_frame.borrow_mut().state = {type_name}State::Completed;
                Self::settle_active(
                    std::rc::Rc::clone(&rejected_frame),
                    false,
                    reason,
                );
            }} else {{
                let suspension = match rejected_frame.borrow().state {{
                    {type_name}State::Suspended(suspension) => suspension,
                    _ => {{
                        Self::settle_active(
                            std::rc::Rc::clone(&rejected_frame),
                            false,
                            reason,
                        );
                        return w3cos_core::Value::Undefined;
                    }}
                }};
                rejected_frame.borrow_mut().reject_yield(suspension, reason);
                Self::continue_active(std::rc::Rc::clone(&rejected_frame));
            }}
            w3cos_core::Value::Undefined
        }});
        let awaited = w3cos_core::intrinsics::await_value(&value);
        w3cos_core::intrinsics::call_method(
            &awaited,
            &w3cos_core::Value::string("then"),
            vec![on_fulfilled, on_rejected],
        );
    }}

    fn continue_active(frame: std::rc::Rc<std::cell::RefCell<Self>>) {{
        match w3cos_core::catch_js(|| {{
            frame
                .borrow_mut()
                .resume(0, w3cos_core::Value::Undefined)
        }}) {{
            Ok(outcome) => Self::handle(frame, outcome),
            Err(reason) => {{
                frame.borrow_mut().state = {type_name}State::Completed;
                Self::settle_active(frame, false, reason);
            }}
        }}
    }}

    fn drive(frame: std::rc::Rc<std::cell::RefCell<Self>>) {{
        let request = {{
            let mut frame = frame.borrow_mut();
            if frame.active.is_some() {{
                return;
            }}
            let Some(request) = frame.queue.pop_front() else {{
                return;
            }};
            let kind = request.kind;
            let input = request.input.clone();
            frame.active = Some(request);
            (kind, input)
        }};
        match w3cos_core::catch_js(|| {{
            frame.borrow_mut().resume(request.0, request.1)
        }}) {{
            Ok(outcome) => Self::handle(frame, outcome),
            Err(reason) => {{
                frame.borrow_mut().state = {type_name}State::Completed;
                Self::settle_active(frame, false, reason);
            }}
        }}
    }}

    fn enqueue(
        frame: std::rc::Rc<std::cell::RefCell<Self>>,
        kind: u8,
        input: w3cos_core::Value,
    ) -> w3cos_core::Value {{
        w3cos_core::intrinsics::promise_new(vec![w3cos_core::Value::function(move |_, arguments| {{
            let resolve = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
            let reject = arguments.get(1).cloned().unwrap_or(w3cos_core::Value::Undefined);
            frame.borrow_mut().queue.push_back({request_name} {{
                kind,
                input: input.clone(),
                resolve,
                reject,
            }});
            Self::drive(std::rc::Rc::clone(&frame));
            w3cos_core::Value::Undefined
        }})])
    }}
}}

pub fn {rust_name}(
    __this: w3cos_core::Value,
    __args: &[w3cos_core::Value],
    __captures: &w3cos_core::AotCaptureMap,
) -> w3cos_core::Value {{
{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        state: {type_name}State::Start,
{registers_init}
{num_init}        bindings,
        capture_getters,
        capture_setters,
        delegate_iterator: None,
        block: {entry},
        queue: std::collections::VecDeque::new(),
        active: None,
    }}));
    let generator = w3cos_core::Value::object(std::collections::HashMap::new());
    for (name, kind) in [("next", 0_u8), ("return", 1_u8), ("throw", 2_u8)] {{
        let frame = std::rc::Rc::clone(&frame);
        generator.set_property(name, w3cos_core::Value::function(move |_, arguments| {{
            let input = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
            {type_name}::enqueue(std::rc::Rc::clone(&frame), kind, input)
        }}));
    }}
    // Return the receiver; capturing `generator` would form a cycle.
    generator.set_property(
        "__w3cos_symbol_async_iterator",
        w3cos_core::Value::function(|this, _| this),
    );
    generator
}}
"#,
        registers_field = emit_registers_field(plan.n_val),
        registers_init = emit_registers_init(plan.n_val),
        entry = function.entry.0,
    ))
}

fn exception_target_for_block(function: &Function, block: BlockId) -> Option<(Register, BlockId)> {
    function
        .exception_regions
        .iter()
        .find(|region| region.protected_blocks.contains(&block))
        .and_then(|region| {
            region
                .catch_block
                .or(region.finally_block)
                .map(|target| (region.exception, target))
        })
}

fn coalesce_sync_blocks(function: &Function) -> Vec<(BlockId, Vec<Instruction>)> {
    let mut predecessors = HashMap::<BlockId, usize>::new();
    for block in &function.blocks {
        match block.instructions.last() {
            Some(Instruction::Jump { target }) => {
                *predecessors.entry(*target).or_default() += 1;
            }
            Some(Instruction::Branch {
                then_block,
                else_block,
                ..
            }) => {
                *predecessors.entry(*then_block).or_default() += 1;
                if then_block != else_block {
                    *predecessors.entry(*else_block).or_default() += 1;
                }
            }
            _ => {}
        }
    }

    let mut roots = HashSet::from([function.entry]);
    for region in &function.exception_regions {
        roots.extend(region.catch_block);
        roots.extend(region.finally_block);
    }
    let blocks_by_id = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    let mut merged_targets = HashSet::new();
    for block in &function.blocks {
        let Some(Instruction::Jump { target }) = block.instructions.last() else {
            continue;
        };
        if predecessors.get(target).copied() == Some(1)
            && !roots.contains(target)
            && exception_target_for_block(function, block.id)
                == exception_target_for_block(function, *target)
        {
            merged_targets.insert(*target);
        }
    }

    let mut output = Vec::new();
    for block in &function.blocks {
        if merged_targets.contains(&block.id) {
            continue;
        }
        let mut instructions = block.instructions.clone();
        let mut visited = HashSet::from([block.id]);
        loop {
            let Some(Instruction::Jump { target }) = instructions.last() else {
                break;
            };
            if !merged_targets.contains(target) || !visited.insert(*target) {
                break;
            }
            let Some(next) = blocks_by_id.get(target) else {
                break;
            };
            instructions.pop();
            instructions.extend(next.instructions.iter().cloned());
        }
        output.push((block.id, instructions));
    }
    output
}

fn successor_live(
    instructions: &[Instruction],
    function: &Function,
    exception_target: Option<(Register, BlockId)>,
    live_in: &HashMap<BlockId, HashSet<u32>>,
) -> HashSet<u32> {
    let mut live = HashSet::new();
    if let Some(last) = instructions.last() {
        for successor in control_successors(last, function) {
            if let Some(incoming) = live_in.get(&successor) {
                live.extend(incoming.iter().copied());
            }
        }
    }
    if let Some((_, catch)) = exception_target
        && let Some(incoming) = live_in.get(&catch)
    {
        live.extend(incoming.iter().copied());
    }
    live
}

fn value_uses(instruction: &Instruction, proven: &HashMap<u32, String>) -> Vec<u32> {
    let mut reads = read_registers(instruction);
    let key = match instruction {
        Instruction::GetProperty { key, .. }
        | Instruction::SetProperty { key, .. }
        | Instruction::DeleteProperty { key, .. }
        | Instruction::CallMethod { key, .. }
        | Instruction::CallMethodWithArguments { key, .. }
            if proven.contains_key(&key.0) =>
        {
            Some(key.0)
        }
        _ => None,
    };
    if let Some(key) = key
        && let Some(position) = reads.iter().position(|register| *register == key)
    {
        reads.remove(position);
    }
    reads
}

/// String loads whose only consumers inline the literal as a property key.
fn dead_string_loads(
    instructions: &[Instruction],
    function: &Function,
    exception_target: Option<(Register, BlockId)>,
    live_in: &HashMap<BlockId, HashSet<u32>>,
) -> HashSet<usize> {
    let end_live = successor_live(instructions, function, exception_target, live_in);
    let mut dead = HashSet::new();
    for (index, instruction) in instructions.iter().enumerate() {
        let Instruction::LoadConstant {
            dst,
            value: Constant::String(_),
        } = instruction
        else {
            continue;
        };
        if end_live.contains(&dst.0) {
            continue;
        }
        let mut needed = false;
        for (later_index, later) in instructions.iter().enumerate().skip(index + 1) {
            let proven = proven_string_keys(instructions, later_index);
            if value_uses(later, &proven).contains(&dst.0) {
                needed = true;
                break;
            }
            if written_register(later).is_some_and(|written| written.0 == dst.0) {
                break;
            }
        }
        if !needed {
            dead.insert(index);
        }
    }
    dead
}

fn emit_block_instructions(
    instructions: &[Instruction],
    exception_target: Option<(w3cos_ir::Register, w3cos_ir::BlockId)>,
    function: &Function,
    factories: &HashMap<FunctionId, String>,
    mode: EmissionMode,
    module_specifier: Option<&str>,
    type_name: &str,
    storage: SlotStorage,
    plan: &EscapePlan,
    back_edges: &UniqueBackEdgePlan,
) -> Result<String> {
    let reaching = reaching_number_constants(instructions);
    let live_in = analyze_live_in(function);
    let last_uses = last_uses_in_sequence(instructions, function, exception_target, &live_in);
    let dead_strings = dead_string_loads(instructions, function, exception_target, &live_in);
    let emitted = instructions
        .iter()
        .enumerate()
        .map(|(index, instruction)| {
            let proven = proven_string_keys(instructions, index);
            emit_instruction(
                instruction,
                exception_target,
                function,
                factories,
                mode,
                module_specifier,
                storage,
                &proven,
                plan,
                back_edges,
                &reaching[index],
                &last_uses[index],
            )
            .map(|emitted| match mode {
                EmissionMode::Async => emitted.replace("__W3COS_ASYNC_FRAME__", type_name),
                EmissionMode::Generator | EmissionMode::AsyncGenerator => {
                    emitted.replace("__W3COS_STATE__", &format!("{type_name}State"))
                }
                EmissionMode::Sync => emitted,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut output = String::new();
    for (index, (instruction, emitted)) in instructions.iter().zip(&emitted).enumerate() {
        if dead_strings.contains(&index) {
            continue;
        }
        let line = if exception_target.is_some()
            && !is_direct_control_flow(instruction, mode)
            && !handles_own_exceptions(instruction)
        {
            emit_caught_stmt(emitted, exception_target, plan)
        } else {
            emitted.clone()
        };
        output.push_str("                    ");
        output.push_str(&line);
        output.push('\n');
    }
    Ok(output)
}

fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Static qualified-name id, or one prologue intern for a dynamic literal.
fn qualified_atom_expr(name: &str) -> String {
    if let Some(id) = w3cos_dom::atom::static_qualified_id(name) {
        format!("{id}_u32")
    } else {
        let hash = fnv1a(name);
        format!("w3cos_core::intrinsics::dom_intern_prehashed({name:?}, {hash}_u64)")
    }
}

struct AppendedShape {
    document_binding: u32,
    parent_binding: u32,
    nodes_binding: u32,
    element_reg: u32,
    index_binding: u32,
    tag: String,
    id_prefix: String,
    class_name: String,
    text_prefix: String,
    store_at: usize,
    tail_at: usize,
}

fn load_binding_inst(inst: &Instruction) -> Option<(u32, u32)> {
    match inst {
        Instruction::LoadBinding { dst, binding } => Some((dst.0, binding.0)),
        _ => None,
    }
}

fn load_string_inst(inst: &Instruction) -> Option<(u32, String)> {
    match inst {
        Instruction::LoadConstant {
            dst,
            value: Constant::String(text),
        } => Some((dst.0, text.clone())),
        _ => None,
    }
}

/// `createElement` + id/class/text + `appendChild` + `push` with one integer index.
fn pull<'a>(body: &'a [Instruction], i: &mut usize) -> Option<&'a Instruction> {
    let inst = body.get(*i)?;
    *i += 1;
    Some(inst)
}

fn match_appended_element(body: &[Instruction]) -> Option<AppendedShape> {
    let mut i = 0usize;
    let (document_reg, document_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (create_key_reg, create_key) = load_string_inst(pull(body, &mut i)?)?;
    if create_key != "createElement" {
        return None;
    }
    let (tag_reg, tag) = load_string_inst(pull(body, &mut i)?)?;
    if tag.is_empty() {
        return None;
    }
    let Instruction::CallMethod {
        dst,
        object,
        key,
        arguments,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != document_reg
        || key.0 != create_key_reg
        || arguments.len() != 1
        || arguments[0].0 != tag_reg
    {
        return None;
    }
    let element_reg = dst.0;
    let store_at = i;
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if value.0 != element_reg {
        return None;
    }
    let element_binding = binding.0;

    let (id_object, id_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if id_bind != element_binding {
        return None;
    }
    let (id_key_reg, id_key) = load_string_inst(pull(body, &mut i)?)?;
    if id_key != "id" {
        return None;
    }
    let (id_prefix_reg, id_prefix) = load_string_inst(pull(body, &mut i)?)?;
    let (id_index_reg, index_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let Instruction::Add { dst, lhs, rhs } = pull(body, &mut i)? else {
        return None;
    };
    if lhs.0 != id_prefix_reg || rhs.0 != id_index_reg {
        return None;
    }
    let id_value = dst.0;
    let Instruction::SetProperty {
        object,
        key,
        value,
        strict: _,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != id_object || key.0 != id_key_reg || value.0 != id_value {
        return None;
    }

    let (class_object, class_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if class_bind != element_binding {
        return None;
    }
    let (class_key_reg, class_key) = load_string_inst(pull(body, &mut i)?)?;
    if class_key != "className" {
        return None;
    }
    let (class_reg, class_name) = load_string_inst(pull(body, &mut i)?)?;
    if class_name.is_empty()
        || class_name
            .bytes()
            .any(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c))
    {
        return None;
    }
    let Instruction::SetProperty {
        object,
        key,
        value,
        strict: _,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != class_object || key.0 != class_key_reg || value.0 != class_reg {
        return None;
    }

    let (text_object, text_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if text_bind != element_binding {
        return None;
    }
    let (text_key_reg, text_key) = load_string_inst(pull(body, &mut i)?)?;
    if text_key != "textContent" {
        return None;
    }
    let (text_prefix_reg, text_prefix) = load_string_inst(pull(body, &mut i)?)?;
    let (text_index_reg, text_index_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if text_index_binding != index_binding {
        return None;
    }
    let Instruction::Add { dst, lhs, rhs } = pull(body, &mut i)? else {
        return None;
    };
    if lhs.0 != text_prefix_reg || rhs.0 != text_index_reg {
        return None;
    }
    let text_value = dst.0;
    let Instruction::SetProperty {
        object,
        key,
        value,
        strict: _,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != text_object || key.0 != text_key_reg || value.0 != text_value {
        return None;
    }

    let (parent_reg, parent_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (append_key_reg, append_key) = load_string_inst(pull(body, &mut i)?)?;
    if append_key != "appendChild" {
        return None;
    }
    let (_, append_child_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if append_child_bind != element_binding {
        return None;
    }
    let Instruction::CallMethod {
        object,
        key,
        arguments,
        ..
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != parent_reg || key.0 != append_key_reg || arguments.len() != 1 {
        return None;
    }

    let (nodes_reg, nodes_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (push_key_reg, push_key) = load_string_inst(pull(body, &mut i)?)?;
    if push_key != "push" {
        return None;
    }
    let (_, push_child_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if push_child_bind != element_binding {
        return None;
    }
    let Instruction::CallMethod {
        object,
        key,
        arguments,
        ..
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != nodes_reg || key.0 != push_key_reg || arguments.len() != 1 {
        return None;
    }
    let tail_at = i;
    let tail_stores_index = body[tail_at..].iter().any(|inst| {
        matches!(inst, Instruction::StoreBinding { binding, .. } if binding.0 == index_binding)
    });
    let tail_is_arithmetic = body[tail_at..].iter().all(|inst| {
        matches!(
            inst,
            Instruction::LoadConstant { .. }
                | Instruction::LoadBinding { .. }
                | Instruction::Add { .. }
                | Instruction::Binary { .. }
                | Instruction::StoreBinding { .. }
                | Instruction::Move { .. }
        )
    });
    if tail_at >= body.len() || !tail_stores_index || !tail_is_arithmetic {
        return None;
    }
    Some(AppendedShape {
        document_binding,
        parent_binding,
        nodes_binding,
        element_reg,
        index_binding,
        tag,
        id_prefix,
        class_name,
        text_prefix,
        store_at,
        tail_at,
    })
}

fn stable_binding_expr(id: u32) -> String {
    format!(
        "if let Some(getter) = __w3cos_cg_{id}.as_ref() {{ match w3cos_core::catch_js(|| getter.call_slice(w3cos_core::Value::Undefined, &[])) {{ Ok(__w3cos_value) => __w3cos_value, Err(__w3cos_exception) => {{ return Err(__w3cos_exception); }} }} }} else if let Some(cell) = __w3cos_cb_{id}.as_ref() {{ let binding = cell.borrow(); if binding.1 {{ binding.0.clone() }} else {{ let __w3cos_exception = w3cos_core::intrinsics::reference_error(\"binding is not initialized\"); return Err(__w3cos_exception); }} }} else {{ let __w3cos_exception = w3cos_core::intrinsics::reference_error(\"binding is not initialized\"); return Err(__w3cos_exception); }}"
    )
}

/// Declarations are pure. Binding reads happen later, after the loop gate.
fn fusion_latch_prelude(suffix: &str, atoms: &str, names: &[&str]) -> String {
    let mut out = format!(
        "let mut __w3cos_fuse_latched_{suffix} = false;\nlet mut __w3cos_fuse_fast_{suffix} = false;\nlet mut __w3cos_dom_epoch_{suffix}: u64 = 0;\n{atoms}"
    );
    for name in names {
        out.push_str(&format!(
            "let mut __w3cos_{name}_{suffix} = w3cos_core::Value::Undefined;\n"
        ));
    }
    out
}

/// Load the fused receivers only once the loop body is known to run, then
/// accept the fast path only when those receivers still expose the sealed
/// builtins. The epoch is sampled after that proof so a later override falls
/// back without treating an earlier `style` prototype install as a mismatch.
fn fusion_latch_open(suffix: &str, names: &[(&str, u32)], surface: &str) -> String {
    let mut out =
        format!("if !__w3cos_fuse_latched_{suffix} {{\n__w3cos_fuse_latched_{suffix} = true;\n");
    for (name, id) in names {
        out.push_str(&format!(
            "__w3cos_{name}_{suffix} = {};\n",
            stable_binding_expr(*id)
        ));
    }
    out.push_str(&format!(
        "if {surface} {{\n__w3cos_dom_epoch_{suffix} = w3cos_core::intrinsics::dom_shape_epoch();\n__w3cos_fuse_fast_{suffix} = true;\n}}\n}}\nif __w3cos_fuse_fast_{suffix} {{\n"
    ));
    out
}

fn fusion_surface_expr(suffix: &str, nodes: bool, tag: &str) -> String {
    let nodes = if nodes {
        format!("Some(&__w3cos_nodes_{suffix})")
    } else {
        "None".to_string()
    };
    format!(
        "w3cos_core::intrinsics::dom_fusion_surface_ok(&__w3cos_doc_{suffix}, &__w3cos_parent_{suffix}, {nodes}, {tag:?})"
    )
}

/// Stamp-guarded fused create. The else branch is the ordinary lookup body.
fn emit_guarded_append(
    body: &[Instruction],
    slow: &str,
    function: &Function,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
    type_name: &str,
    storage: SlotStorage,
    plan: &EscapePlan,
    back_edges: &UniqueBackEdgePlan,
) -> Result<Option<(String, String)>> {
    let Some(shape) = match_appended_element(body) else {
        return Ok(None);
    };
    let Some(index_slot) = num_binding_slot(plan, shape.index_binding) else {
        return Ok(None);
    };
    let Some(element_slot) = val_slot(plan, shape.element_reg) else {
        return Ok(None);
    };
    let store = emit_block_instructions(
        &body[shape.store_at..shape.store_at + 1],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let tail = emit_block_instructions(
        &body[shape.tail_at..],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let tag_atom = qualified_atom_expr(&shape.tag);
    let class_atom = qualified_atom_expr(&shape.class_name);
    let suffix = format!(
        "a{}_{}_{}",
        shape.document_binding, shape.parent_binding, shape.nodes_binding
    );
    let prelude = fusion_latch_prelude(
        &suffix,
        &format!(
            "let __w3cos_tag_atom_{suffix}: u32 = {tag_atom};\nlet __w3cos_class_atom_{suffix}: u32 = {class_atom};\n"
        ),
        &["doc", "parent", "nodes"],
    );
    let open = fusion_latch_open(
        &suffix,
        &[
            ("doc", shape.document_binding),
            ("parent", shape.parent_binding),
            ("nodes", shape.nodes_binding),
        ],
        &fusion_surface_expr(&suffix, true, &shape.tag),
    );
    let fast = format!(
        "{open}let __w3cos_built = w3cos_core::intrinsics::dom_build_guarded(&__w3cos_doc_{suffix}, &__w3cos_parent_{suffix}, {tag:?}, __w3cos_tag_atom_{suffix}, {id_prefix:?}, self.num_bindings[{index_slot}], {class_name:?}, __w3cos_class_atom_{suffix}, {text_prefix:?}, self.num_bindings[{index_slot}], __w3cos_dom_epoch_{suffix});\nif !__w3cos_built.is_undefined() {{\nself.registers[{element_slot}] = __w3cos_built;\n{store}w3cos_core::intrinsics::array_push(&__w3cos_nodes_{suffix}, &self.registers[{element_slot}]);\n{tail}}} else {{\n{slow}}}\n}} else {{\n{slow}}}\n",
        tag = shape.tag,
        id_prefix = shape.id_prefix,
        class_name = shape.class_name,
        text_prefix = shape.text_prefix,
    );
    Ok(Some((prelude, fast)))
}

struct StyledBlock {
    document_binding: u32,
    parent_binding: u32,
    element_reg: u32,
    text_reg: u32,
    tag: String,
    style_name: String,
    style_value: String,
    /// `flex` value when the row is `style.height` then `style.flex`.
    flex_value: Option<String>,
    text_expr_at: usize,
    text_expr_end: usize,
    store_at: usize,
    tail_at: usize,
}

fn pure_value_inst(inst: &Instruction) -> bool {
    matches!(
        inst,
        Instruction::LoadConstant { .. }
            | Instruction::LoadBinding { .. }
            | Instruction::Add { .. }
            | Instruction::Binary { .. }
            | Instruction::Unary { .. }
            | Instruction::Move { .. }
    )
}

/// One `element.style.name = "value"` with both sides constant.
///
/// A miss restores `i` so the caller can parse `appendChild` instead.
fn take_constant_style(
    body: &[Instruction],
    i: &mut usize,
    element_binding: u32,
) -> Option<(String, String)> {
    let saved = *i;
    let parsed = parse_constant_style(body, i, element_binding);
    if parsed.is_none() {
        *i = saved;
    }
    parsed
}

fn parse_constant_style(
    body: &[Instruction],
    i: &mut usize,
    element_binding: u32,
) -> Option<(String, String)> {
    let (style_object, style_bind) = load_binding_inst(pull(body, i)?)?;
    if style_bind != element_binding {
        return None;
    }
    let (style_key_reg, style_key) = load_string_inst(pull(body, i)?)?;
    if style_key != "style" {
        return None;
    }
    let Instruction::GetProperty {
        dst: style_dst,
        object,
        key,
    } = pull(body, i)?
    else {
        return None;
    };
    if object.0 != style_object || key.0 != style_key_reg {
        return None;
    }
    let style_reg = style_dst.0;
    let (prop_reg, style_name) = load_string_inst(pull(body, i)?)?;
    let (value_reg, style_value) = load_string_inst(pull(body, i)?)?;
    if style_name.is_empty() || style_value.is_empty() {
        return None;
    }
    let Instruction::SetProperty {
        object,
        key,
        value,
        strict: _,
    } = pull(body, i)?
    else {
        return None;
    };
    if object.0 != style_reg || key.0 != prop_reg || value.0 != value_reg {
        return None;
    }
    Some((style_name, style_value))
}

/// `createElement` + `textContent` + one constant `style.*` + `appendChild`.
///
/// A second assignment `style.height` then `style.flex` is the column-flex
/// row shape. Any other second property stays on the ordinary lookup.
fn match_styled_block(body: &[Instruction]) -> Option<StyledBlock> {
    let mut i = 0usize;
    let (document_reg, document_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (create_key_reg, create_key) = load_string_inst(pull(body, &mut i)?)?;
    if create_key != "createElement" {
        return None;
    }
    let (tag_reg, tag) = load_string_inst(pull(body, &mut i)?)?;
    if tag.is_empty() {
        return None;
    }
    let Instruction::CallMethod {
        dst,
        object,
        key,
        arguments,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != document_reg
        || key.0 != create_key_reg
        || arguments.len() != 1
        || arguments[0].0 != tag_reg
    {
        return None;
    }
    let element_reg = dst.0;
    let store_at = i;
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if value.0 != element_reg {
        return None;
    }
    let element_binding = binding.0;

    let (text_object, text_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if text_bind != element_binding {
        return None;
    }
    let (text_key_reg, text_key) = load_string_inst(pull(body, &mut i)?)?;
    if text_key != "textContent" {
        return None;
    }
    let text_expr_at = i;
    let mut text_reg = None;
    while i < body.len() {
        if let Instruction::SetProperty {
            object,
            key,
            value,
            strict: _,
        } = &body[i]
        {
            if object.0 == text_object && key.0 == text_key_reg {
                text_reg = Some(value.0);
                break;
            }
        }
        if !pure_value_inst(&body[i]) {
            return None;
        }
        i += 1;
    }
    let text_reg = text_reg?;
    let text_expr_end = i;
    i += 1;

    let (style_object, style_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if style_bind != element_binding {
        return None;
    }
    let (style_key_reg, style_key) = load_string_inst(pull(body, &mut i)?)?;
    if style_key != "style" {
        return None;
    }
    let Instruction::GetProperty {
        dst: style_dst,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != style_object || key.0 != style_key_reg {
        return None;
    }
    let style_reg = style_dst.0;
    let (prop_reg, style_name) = load_string_inst(pull(body, &mut i)?)?;
    let (value_reg, style_value) = load_string_inst(pull(body, &mut i)?)?;
    if style_name.is_empty() || style_value.is_empty() {
        return None;
    }
    let Instruction::SetProperty {
        object,
        key,
        value,
        strict: _,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != style_reg || key.0 != prop_reg || value.0 != value_reg {
        return None;
    }
    let flex_value = match take_constant_style(body, &mut i, element_binding) {
        Some((name, value)) if style_name == "height" && name == "flex" => Some(value),
        Some(_) => return None,
        None => None,
    };

    let (parent_reg, parent_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (append_key_reg, append_key) = load_string_inst(pull(body, &mut i)?)?;
    if append_key != "appendChild" {
        return None;
    }
    let (_, append_child_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if append_child_bind != element_binding {
        return None;
    }
    let Instruction::CallMethod {
        object,
        key,
        arguments,
        ..
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != parent_reg || key.0 != append_key_reg || arguments.len() != 1 {
        return None;
    }
    let tail_at = i;
    let tail_is_arithmetic = !body[tail_at..].is_empty()
        && body[tail_at..].iter().all(|inst| {
            matches!(
                inst,
                Instruction::LoadConstant { .. }
                    | Instruction::LoadBinding { .. }
                    | Instruction::Add { .. }
                    | Instruction::Binary { .. }
                    | Instruction::Unary { .. }
                    | Instruction::StoreBinding { .. }
                    | Instruction::Move { .. }
            )
        });
    if !tail_is_arithmetic {
        return None;
    }
    Some(StyledBlock {
        document_binding,
        parent_binding,
        element_reg,
        text_reg,
        tag,
        style_name,
        style_value,
        flex_value,
        text_expr_at,
        text_expr_end,
        store_at,
        tail_at,
    })
}

fn emit_guarded_block(
    body: &[Instruction],
    slow: &str,
    function: &Function,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
    type_name: &str,
    storage: SlotStorage,
    plan: &EscapePlan,
    back_edges: &UniqueBackEdgePlan,
) -> Result<Option<(String, String)>> {
    let Some(shape) = match_styled_block(body) else {
        return Ok(None);
    };
    let Some(element_slot) = val_slot(plan, shape.element_reg) else {
        return Ok(None);
    };
    let Some(text_slot) = val_slot(plan, shape.text_reg) else {
        return Ok(None);
    };
    let text_expr = emit_block_instructions(
        &body[shape.text_expr_at..shape.text_expr_end],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let store = emit_block_instructions(
        &body[shape.store_at..shape.store_at + 1],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let tail = emit_block_instructions(
        &body[shape.tail_at..],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let tag_atom = qualified_atom_expr(&shape.tag);
    let suffix = format!("b{}_{}", shape.document_binding, shape.parent_binding);
    let prelude = fusion_latch_prelude(
        &suffix,
        &format!("let __w3cos_tag_atom_{suffix}: u32 = {tag_atom};\n"),
        &["doc", "parent"],
    );
    let open = fusion_latch_open(
        &suffix,
        &[
            ("doc", shape.document_binding),
            ("parent", shape.parent_binding),
        ],
        &fusion_surface_expr(&suffix, false, &shape.tag),
    );
    let call = if let Some(flex_value) = &shape.flex_value {
        format!(
            "w3cos_core::intrinsics::dom_build_flex_row_guarded(&__w3cos_doc_{suffix}, &__w3cos_parent_{suffix}, {tag:?}, __w3cos_tag_atom_{suffix}, &self.registers[{text_slot}], {height:?}, {flex_value:?}, __w3cos_dom_epoch_{suffix})",
            tag = shape.tag,
            height = shape.style_value,
        )
    } else {
        format!(
            "w3cos_core::intrinsics::dom_build_block_guarded(&__w3cos_doc_{suffix}, &__w3cos_parent_{suffix}, {tag:?}, __w3cos_tag_atom_{suffix}, &self.registers[{text_slot}], {style_name:?}, {style_value:?}, __w3cos_dom_epoch_{suffix})",
            tag = shape.tag,
            style_name = shape.style_name,
            style_value = shape.style_value,
        )
    };
    let fast = format!(
        "{open}{text_expr}let __w3cos_built = {call};\nif !__w3cos_built.is_undefined() {{\nself.registers[{element_slot}] = __w3cos_built;\n{store}{tail}}} else {{\n{slow}}}\n}} else {{\n{slow}}}\n",
    );
    Ok(Some((prelude, fast)))
}

struct TextAppend {
    document_binding: u32,
    parent_binding: u32,
    element_reg: u32,
    text_reg: u32,
    tag: String,
    text_expr_at: usize,
    text_expr_end: usize,
    store_at: usize,
    tail_at: usize,
}

/// `createElement` + `textContent` + `appendChild`, with no inline style.
///
/// A following `style.*` assignment returns `None` so the styled-block matcher
/// keeps height, flex, and every other declaration.
fn match_text_append(body: &[Instruction]) -> Option<TextAppend> {
    let mut i = 0usize;
    let (document_reg, document_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (create_key_reg, create_key) = load_string_inst(pull(body, &mut i)?)?;
    if create_key != "createElement" {
        return None;
    }
    let (tag_reg, tag) = load_string_inst(pull(body, &mut i)?)?;
    if tag.is_empty() {
        return None;
    }
    let Instruction::CallMethod {
        dst,
        object,
        key,
        arguments,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != document_reg
        || key.0 != create_key_reg
        || arguments.len() != 1
        || arguments[0].0 != tag_reg
    {
        return None;
    }
    let element_reg = dst.0;
    let store_at = i;
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if value.0 != element_reg {
        return None;
    }
    let element_binding = binding.0;

    let (text_object, text_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if text_bind != element_binding {
        return None;
    }
    let (text_key_reg, text_key) = load_string_inst(pull(body, &mut i)?)?;
    if text_key != "textContent" {
        return None;
    }
    let text_expr_at = i;
    let mut text_reg = None;
    while i < body.len() {
        if let Instruction::SetProperty {
            object,
            key,
            value,
            strict: _,
        } = &body[i]
        {
            if object.0 == text_object && key.0 == text_key_reg {
                text_reg = Some(value.0);
                break;
            }
        }
        if !pure_value_inst(&body[i]) {
            return None;
        }
        i += 1;
    }
    let text_reg = text_reg?;
    let text_expr_end = i;
    i += 1;

    let (parent_reg, parent_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (append_key_reg, append_key) = load_string_inst(pull(body, &mut i)?)?;
    if append_key != "appendChild" {
        return None;
    }
    let (_, append_child_bind) = load_binding_inst(pull(body, &mut i)?)?;
    if append_child_bind != element_binding {
        return None;
    }
    let Instruction::CallMethod {
        object,
        key,
        arguments,
        ..
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != parent_reg || key.0 != append_key_reg || arguments.len() != 1 {
        return None;
    }
    let tail_at = i;
    let tail_is_arithmetic = !body[tail_at..].is_empty()
        && body[tail_at..].iter().all(|inst| {
            matches!(
                inst,
                Instruction::LoadConstant { .. }
                    | Instruction::LoadBinding { .. }
                    | Instruction::Add { .. }
                    | Instruction::Binary { .. }
                    | Instruction::Unary { .. }
                    | Instruction::StoreBinding { .. }
                    | Instruction::Move { .. }
            )
        });
    if !tail_is_arithmetic {
        return None;
    }
    Some(TextAppend {
        document_binding,
        parent_binding,
        element_reg,
        text_reg,
        tag,
        text_expr_at,
        text_expr_end,
        store_at,
        tail_at,
    })
}

fn emit_guarded_text(
    body: &[Instruction],
    slow: &str,
    function: &Function,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
    type_name: &str,
    storage: SlotStorage,
    plan: &EscapePlan,
    back_edges: &UniqueBackEdgePlan,
) -> Result<Option<(String, String)>> {
    let Some(shape) = match_text_append(body) else {
        return Ok(None);
    };
    let Some(element_slot) = val_slot(plan, shape.element_reg) else {
        return Ok(None);
    };
    let Some(text_slot) = val_slot(plan, shape.text_reg) else {
        return Ok(None);
    };
    let text_expr = emit_block_instructions(
        &body[shape.text_expr_at..shape.text_expr_end],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let store = emit_block_instructions(
        &body[shape.store_at..shape.store_at + 1],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let tail = emit_block_instructions(
        &body[shape.tail_at..],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let tag_atom = qualified_atom_expr(&shape.tag);
    let suffix = format!("t{}_{}", shape.document_binding, shape.parent_binding);
    let prelude = fusion_latch_prelude(
        &suffix,
        &format!("let __w3cos_tag_atom_{suffix}: u32 = {tag_atom};\n"),
        &["doc", "parent"],
    );
    let open = fusion_latch_open(
        &suffix,
        &[
            ("doc", shape.document_binding),
            ("parent", shape.parent_binding),
        ],
        &fusion_surface_expr(&suffix, false, &shape.tag),
    );
    let fast = format!(
        "{open}{text_expr}let __w3cos_built = w3cos_core::intrinsics::dom_build_text_guarded(&__w3cos_doc_{suffix}, &__w3cos_parent_{suffix}, {tag:?}, __w3cos_tag_atom_{suffix}, &self.registers[{text_slot}], __w3cos_dom_epoch_{suffix});\nif !__w3cos_built.is_undefined() {{\nself.registers[{element_slot}] = __w3cos_built;\n{store}{tail}}} else {{\n{slow}}}\n}} else {{\n{slow}}}\n",
        tag = shape.tag,
    );
    Ok(Some((prelude, fast)))
}

fn load_number_inst(inst: &Instruction) -> Option<(u32, f64)> {
    match inst {
        Instruction::LoadConstant {
            dst,
            value: Constant::Number(number),
        } if number.is_finite() => Some((dst.0, *number)),
        _ => None,
    }
}

struct IndexedRewrite {
    k_binding: u32,
    limit_binding: u32,
    modulus_binding: u32,
    nodes_binding: u32,
    parent_binding: u32,
    text_sum_binding: u32,
    stride: f64,
    prefix: String,
}

/// One strict `nodes[(k * stride) % modulus].textContent = prefix + k` move.
///
/// The header is `k < limit` and the body ends with `k = k * 1 + 1`. Anything
/// else, including an extra property write, stays on the ordinary path.
fn match_indexed_text_rewrite(
    header: &[Instruction],
    body: &[Instruction],
) -> Option<IndexedRewrite> {
    let header = match header.last() {
        Some(Instruction::Branch { .. }) => &header[..header.len() - 1],
        _ => return None,
    };
    let mut i = 0usize;
    let (k_reg, k_binding) = load_binding_inst(pull(header, &mut i)?)?;
    let (limit_reg, limit_binding) = load_binding_inst(pull(header, &mut i)?)?;
    let Instruction::Binary {
        operator: BinaryOperator::LessThan,
        lhs,
        rhs,
        ..
    } = pull(header, &mut i)?
    else {
        return None;
    };
    if lhs.0 != k_reg || rhs.0 != limit_reg || i != header.len() {
        return None;
    }

    i = 0;
    let (k_body, k_body_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if k_body_binding != k_binding {
        return None;
    }
    let (stride_reg, stride) = load_number_inst(pull(body, &mut i)?)?;
    if stride.fract() != 0.0 || !(1.0..1_000_001.0).contains(&stride) {
        return None;
    }
    let Instruction::Binary {
        dst: product,
        operator: BinaryOperator::Multiply,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != k_body || rhs.0 != stride_reg {
        return None;
    }
    let (modulus_reg, modulus_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let Instruction::Binary {
        dst: remainder,
        operator: BinaryOperator::Remainder,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != product.0 || rhs.0 != modulus_reg {
        return None;
    }
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if value.0 != remainder.0 {
        return None;
    }
    let index_binding = binding.0;

    let (nodes_reg, nodes_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (index_reg, index_check) = load_binding_inst(pull(body, &mut i)?)?;
    if index_check != index_binding {
        return None;
    }
    let Instruction::GetProperty {
        dst: element,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != nodes_reg || key.0 != index_reg {
        return None;
    }
    let (text_key_reg, text_key) = load_string_inst(pull(body, &mut i)?)?;
    if text_key != "textContent" {
        return None;
    }
    let (prefix_reg, prefix) = load_string_inst(pull(body, &mut i)?)?;
    if !prefix.is_ascii() {
        return None;
    }
    let (k_text, k_text_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if k_text_binding != k_binding {
        return None;
    }
    let Instruction::Add {
        dst: text_value,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != prefix_reg || rhs.0 != k_text {
        return None;
    }
    let Instruction::SetProperty {
        object,
        key,
        value,
        strict,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if !strict || object.0 != element.0 || key.0 != text_key_reg || value.0 != text_value.0 {
        return None;
    }

    let (parent_reg, parent_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (append_key_reg, append_key) = load_string_inst(pull(body, &mut i)?)?;
    if append_key != "appendChild" {
        return None;
    }
    let (nodes_again, nodes_again_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if nodes_again_binding != nodes_binding {
        return None;
    }
    let (index_again, index_again_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if index_again_binding != index_binding {
        return None;
    }
    let Instruction::GetProperty {
        dst: moved,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != nodes_again || key.0 != index_again {
        return None;
    }
    let Instruction::CallMethod {
        object,
        key,
        arguments,
        ..
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != parent_reg
        || key.0 != append_key_reg
        || arguments.len() != 1
        || arguments[0].0 != moved.0
    {
        return None;
    }

    let (sum_reg, text_sum_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (nodes_len, nodes_len_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if nodes_len_binding != nodes_binding {
        return None;
    }
    let (index_len, index_len_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if index_len_binding != index_binding {
        return None;
    }
    let Instruction::GetProperty {
        dst: length_element,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != nodes_len || key.0 != index_len {
        return None;
    }
    let (length_key_reg, length_key) = load_string_inst(pull(body, &mut i)?)?;
    if length_key != "textContent" {
        return None;
    }
    let Instruction::GetProperty {
        dst: length_text,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != length_element.0 || key.0 != length_key_reg {
        return None;
    }
    let (length_name_reg, length_name) = load_string_inst(pull(body, &mut i)?)?;
    if length_name != "length" {
        return None;
    }
    let Instruction::GetProperty {
        dst: length_value,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != length_text.0 || key.0 != length_name_reg {
        return None;
    }
    let Instruction::Add {
        dst: summed,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != sum_reg || rhs.0 != length_value.0 {
        return None;
    }
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if binding.0 != text_sum_binding || value.0 != summed.0 {
        return None;
    }

    let (one_reg, one) = load_number_inst(pull(body, &mut i)?)?;
    if one != 1.0 {
        return None;
    }
    let (k_inc, k_inc_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if k_inc_binding != k_binding {
        return None;
    }
    let Instruction::Binary {
        dst: scaled,
        operator: BinaryOperator::Multiply,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != k_inc || rhs.0 != one_reg {
        return None;
    }
    let Instruction::Add {
        dst: next_k,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != scaled.0 || rhs.0 != one_reg {
        return None;
    }
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if binding.0 != k_binding || value.0 != next_k.0 || i != body.len() {
        return None;
    }
    Some(IndexedRewrite {
        k_binding,
        limit_binding,
        modulus_binding,
        nodes_binding,
        parent_binding,
        text_sum_binding,
        stride,
        prefix,
    })
}

fn binding_number_expr(plan: &EscapePlan, binding: u32) -> String {
    if let Some(number) = plan.constants.get(&binding) {
        return format!("{number:?}");
    }
    if let Some(slot) = num_binding_slot(plan, binding) {
        format!("self.num_bindings[{slot}]")
    } else if is_shared_number(binding) {
        format!("self.num_cells.get(&{binding}).and_then(|cell| cell.get()).unwrap_or(f64::NAN)")
    } else {
        format!("({}).to_number()", stable_binding_expr(binding))
    }
}

fn emit_store_number_binding(plan: &EscapePlan, binding: u32, number_expr: &str) -> String {
    if let Some(slot) = num_binding_slot(plan, binding) {
        return format!("self.num_bindings[{slot}] = {number_expr};");
    }
    if is_shared_number(binding) {
        return format!(
            "if let Some(cell) = self.num_cells.get(&{binding}) {{ cell.set(Some({number_expr})); }}"
        );
    }
    format!(
        "if let Some(setter) = self.capture_setters.get(&{binding}) {{ let __w3cos_stored = w3cos_core::Value::Number({number_expr}); setter.call_slice(w3cos_core::Value::Undefined, std::slice::from_ref(&__w3cos_stored)); }} else if let Some(cell) = self.bindings.get(&{binding}) {{ let mut binding = cell.borrow_mut(); if binding.1 {{ binding.0 = w3cos_core::Value::Number({number_expr}); }} }}"
    )
}

fn emit_add_number_binding(plan: &EscapePlan, binding: u32, addend: &str) -> String {
    if let Some(slot) = num_binding_slot(plan, binding) {
        return format!("self.num_bindings[{slot}] += {addend};");
    }
    if is_shared_number(binding) {
        return format!(
            "if let Some(cell) = self.num_cells.get(&{binding}) {{ cell.set(Some(cell.get().unwrap_or(0.0_f64) + ({addend}))); }}"
        );
    }
    format!(
        "if let Some(setter) = self.capture_setters.get(&{binding}) {{ let __w3cos_current = if let Some(getter) = self.capture_getters.get(&{binding}) {{ getter.call_slice(w3cos_core::Value::Undefined, &[]).to_number() }} else {{ 0.0_f64 }}; let __w3cos_stored = w3cos_core::Value::Number(__w3cos_current + ({addend})); setter.call_slice(w3cos_core::Value::Undefined, std::slice::from_ref(&__w3cos_stored)); }} else if let Some(cell) = self.bindings.get(&{binding}) {{ let mut binding = cell.borrow_mut(); if binding.1 {{ binding.0 = w3cos_core::Value::Number(binding.0.to_number() + ({addend})); }} }}"
    )
}

/// One call for the whole loop, after the gate proves the body runs.
///
/// `Undefined` means the runtime refused before the first write, so the
/// ordinary property path still observes every check.
fn emit_indexed_text_rewrite(
    header: &[Instruction],
    body: &[Instruction],
    slow: &str,
    plan: &EscapePlan,
) -> Option<(String, String)> {
    let shape = match_indexed_text_rewrite(header, body)?;
    let suffix = format!(
        "w{}_{}_{}",
        shape.k_binding, shape.nodes_binding, shape.parent_binding
    );
    let modulus = binding_number_expr(plan, shape.modulus_binding);
    let start = binding_number_expr(plan, shape.k_binding);
    let limit = binding_number_expr(plan, shape.limit_binding);
    let add_sum =
        emit_add_number_binding(plan, shape.text_sum_binding, "__w3cos_rewrite.to_number()");
    let set_k = emit_store_number_binding(plan, shape.k_binding, &limit);
    let prelude = format!("let mut __w3cos_rewrite_tried_{suffix} = false;\n");
    let fast = format!(
        "if !__w3cos_rewrite_tried_{suffix} {{\n__w3cos_rewrite_tried_{suffix} = true;\nlet __w3cos_rewrite_nodes_{suffix} = {};\nlet __w3cos_rewrite_parent_{suffix} = {};\nlet __w3cos_rewrite_epoch_{suffix} = w3cos_core::intrinsics::dom_shape_epoch();\nlet __w3cos_rewrite = w3cos_core::intrinsics::dom_rewrite_indexed_text_guarded(&__w3cos_rewrite_nodes_{suffix}, &__w3cos_rewrite_parent_{suffix}, {prefix:?}, {stride:?}, {modulus}, {start}, {limit}, __w3cos_rewrite_epoch_{suffix});\nif !__w3cos_rewrite.is_undefined() {{\n{add_sum}\n{set_k}\nbreak;\n}}\n}}\n{slow}",
        stable_binding_expr(shape.nodes_binding),
        stable_binding_expr(shape.parent_binding),
        prefix = shape.prefix,
        stride = shape.stride,
    );
    Some((prelude, fast))
}

struct SequentialFill {
    index_binding: u32,
    nodes_binding: u32,
    left_binding: u32,
    text_sum_binding: u32,
    length_reg: u32,
    prefix: String,
    mid: String,
}

/// `nodes[i].textContent = prefix + left + mid + i` plus a length sum.
///
/// The header is `i < nodes.length` and the body ends with `i = i * 1 + 1`.
/// The pieces must be ASCII so the fused length matches the JS string length.
fn match_sequential_text_fill(
    header: &[Instruction],
    body: &[Instruction],
) -> Option<SequentialFill> {
    let header = match header.last() {
        Some(Instruction::Branch { .. }) => &header[..header.len() - 1],
        _ => return None,
    };
    let mut i = 0usize;
    let (index_reg, index_binding) = load_binding_inst(pull(header, &mut i)?)?;
    let (nodes_reg, nodes_binding) = load_binding_inst(pull(header, &mut i)?)?;
    let (length_key_reg, length_key) = load_string_inst(pull(header, &mut i)?)?;
    if length_key != "length" {
        return None;
    }
    let Instruction::GetProperty {
        dst: length_value,
        object,
        key,
    } = pull(header, &mut i)?
    else {
        return None;
    };
    if object.0 != nodes_reg || key.0 != length_key_reg {
        return None;
    }
    let Instruction::Binary {
        operator: BinaryOperator::LessThan,
        lhs,
        rhs,
        ..
    } = pull(header, &mut i)?
    else {
        return None;
    };
    if lhs.0 != index_reg || rhs.0 != length_value.0 || i != header.len() {
        return None;
    }

    i = 0;
    let (prefix_reg, prefix) = load_string_inst(pull(body, &mut i)?)?;
    if !prefix.is_ascii() {
        return None;
    }
    let (left_reg, left_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let Instruction::Add {
        dst: left_text,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != prefix_reg || rhs.0 != left_reg {
        return None;
    }
    let (mid_reg, mid) = load_string_inst(pull(body, &mut i)?)?;
    if !mid.is_ascii() {
        return None;
    }
    let Instruction::Add {
        dst: mid_text,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != left_text.0 || rhs.0 != mid_reg {
        return None;
    }
    let (index_text, index_text_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if index_text_binding != index_binding {
        return None;
    }
    let Instruction::Add {
        dst: text_value,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != mid_text.0 || rhs.0 != index_text {
        return None;
    }
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if value.0 != text_value.0 {
        return None;
    }
    let text_binding = binding.0;

    let (nodes_body, nodes_body_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if nodes_body_binding != nodes_binding {
        return None;
    }
    let (index_get, index_get_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if index_get_binding != index_binding {
        return None;
    }
    let Instruction::GetProperty {
        dst: element,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != nodes_body || key.0 != index_get {
        return None;
    }
    let (text_key_reg, text_key) = load_string_inst(pull(body, &mut i)?)?;
    if text_key != "textContent" {
        return None;
    }
    let (text_reg, text_check) = load_binding_inst(pull(body, &mut i)?)?;
    if text_check != text_binding {
        return None;
    }
    let Instruction::SetProperty {
        object,
        key,
        value,
        strict,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if !strict || object.0 != element.0 || key.0 != text_key_reg || value.0 != text_reg {
        return None;
    }

    let (sum_reg, text_sum_binding) = load_binding_inst(pull(body, &mut i)?)?;
    let (text_len, text_len_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if text_len_binding != text_binding {
        return None;
    }
    let (length_name_reg, length_name) = load_string_inst(pull(body, &mut i)?)?;
    if length_name != "length" {
        return None;
    }
    let Instruction::GetProperty {
        dst: length_chars,
        object,
        key,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if object.0 != text_len || key.0 != length_name_reg {
        return None;
    }
    let Instruction::Add {
        dst: summed,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != sum_reg || rhs.0 != length_chars.0 {
        return None;
    }
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if binding.0 != text_sum_binding || value.0 != summed.0 {
        return None;
    }

    let (one_reg, one) = load_number_inst(pull(body, &mut i)?)?;
    if one != 1.0 {
        return None;
    }
    let (index_inc, index_inc_binding) = load_binding_inst(pull(body, &mut i)?)?;
    if index_inc_binding != index_binding {
        return None;
    }
    let Instruction::Binary {
        dst: scaled,
        operator: BinaryOperator::Multiply,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != index_inc || rhs.0 != one_reg {
        return None;
    }
    let Instruction::Add {
        dst: next_index,
        lhs,
        rhs,
    } = pull(body, &mut i)?
    else {
        return None;
    };
    if lhs.0 != scaled.0 || rhs.0 != one_reg {
        return None;
    }
    let Instruction::StoreBinding { binding, value } = pull(body, &mut i)? else {
        return None;
    };
    if binding.0 != index_binding || value.0 != next_index.0 || i != body.len() {
        return None;
    }
    Some(SequentialFill {
        index_binding,
        nodes_binding,
        left_binding,
        text_sum_binding,
        length_reg: length_value.0,
        prefix,
        mid,
    })
}

fn emit_f64_expr(plan: &EscapePlan, register: u32) -> String {
    if let Some(slot) = num_slot(plan, register) {
        format!("self.num_regs[{slot}]")
    } else {
        format!("{}.to_number()", emit_boxed_value(plan, register))
    }
}

fn emit_sequential_text_fill(
    header: &[Instruction],
    body: &[Instruction],
    slow: &str,
    plan: &EscapePlan,
) -> Option<(String, String)> {
    let shape = match_sequential_text_fill(header, body)?;
    let suffix = format!("f{}_{}", shape.index_binding, shape.nodes_binding);
    let start = binding_number_expr(plan, shape.index_binding);
    let left = binding_number_expr(plan, shape.left_binding);
    let limit = emit_f64_expr(plan, shape.length_reg);
    let add_sum = emit_add_number_binding(plan, shape.text_sum_binding, "__w3cos_fill.to_number()");
    let set_index = emit_store_number_binding(plan, shape.index_binding, &limit);
    let prelude = format!("let mut __w3cos_fill_tried_{suffix} = false;\n");
    let fast = format!(
        "if !__w3cos_fill_tried_{suffix} {{\n__w3cos_fill_tried_{suffix} = true;\nlet __w3cos_fill_nodes_{suffix} = {};\nlet __w3cos_fill_epoch_{suffix} = w3cos_core::intrinsics::dom_shape_epoch();\nlet __w3cos_fill = w3cos_core::intrinsics::dom_fill_sequential_text_guarded(&__w3cos_fill_nodes_{suffix}, {prefix:?}, {left}, {mid:?}, {start}, {limit}, __w3cos_fill_epoch_{suffix});\nif !__w3cos_fill.is_undefined() {{\n{add_sum}\n{set_index}\nbreak;\n}}\n}}\n{slow}",
        stable_binding_expr(shape.nodes_binding),
        prefix = shape.prefix,
        mid = shape.mid,
    );
    Some((prelude, fast))
}

fn emit_native_sync_loop(
    header: &[Instruction],
    blocks: &[(w3cos_ir::BlockId, Vec<Instruction>)],
    native: &NativeLoop,
    function: &Function,
    factories: &HashMap<FunctionId, String>,
    module_specifier: Option<&str>,
    type_name: &str,
    storage: SlotStorage,
    plan: &EscapePlan,
    back_edges: &UniqueBackEdgePlan,
) -> Result<Option<String>> {
    let Some(Instruction::Branch { condition, .. }) = header.last() else {
        return Ok(None);
    };
    let Some(body) = blocks
        .iter()
        .find(|(id, _)| *id == native.body)
        .map(|(_, instructions)| instructions.as_slice())
    else {
        return Ok(None);
    };
    let header_code = emit_block_instructions(
        &header[..header.len() - 1],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let body_code = emit_block_instructions(
        &body[..body.len() - 1],
        None,
        function,
        factories,
        EmissionMode::Sync,
        module_specifier,
        type_name,
        storage,
        plan,
        back_edges,
    )?;
    let condition = emit_branch_condition(plan, condition.0);
    let gate = if native.then_is_body {
        format!("if !({condition}) {{ break; }}")
    } else {
        format!("if {condition} {{ break; }}")
    };
    // Fuse only after the loop gate, and only when the loaded receivers still
    // match the sealed builtins. Binding reads stay in the body so a zero-trip
    // loop does not observe them. A failed surface check or a bumped epoch
    // falls through to `body_code`.
    let body_insts = &body[..body.len() - 1];
    let rewrite = emit_indexed_text_rewrite(header, body_insts, &body_code, plan)
        .or_else(|| emit_sequential_text_fill(header, body_insts, &body_code, plan));
    let guarded = if rewrite.is_some() {
        None
    } else {
        match emit_guarded_append(
            body_insts,
            &body_code,
            function,
            factories,
            module_specifier,
            type_name,
            storage,
            plan,
            back_edges,
        )? {
            some @ Some(_) => some,
            None => match emit_guarded_block(
                body_insts,
                &body_code,
                function,
                factories,
                module_specifier,
                type_name,
                storage,
                plan,
                back_edges,
            )? {
                some @ Some(_) => some,
                None => emit_guarded_text(
                    body_insts,
                    &body_code,
                    function,
                    factories,
                    module_specifier,
                    type_name,
                    storage,
                    plan,
                    back_edges,
                )?,
            },
        }
    };
    let fused = rewrite.as_ref().or(guarded.as_ref());
    let loop_body = if let Some((_, fast)) = fused {
        format!("{header_code}{gate}\n{fast}")
    } else {
        format!("{header_code}{gate}\n{body_code}")
    };
    if loop_body.contains("continue 'drive") || loop_body.contains("self.block") {
        return Ok(None);
    }
    // A per-iteration environment replaces its Rc cell. Initializers can also
    // install a previously missing cell. Neither lookup is invariant.
    let replaced_bindings = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::RefreshBinding { binding }
            | Instruction::InitializeBinding { binding, .. } => Some(binding.0),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let (mut hoisted, loop_body) = tighten_native_loop(&loop_body, &replaced_bindings);
    if let Some((prelude, _)) = fused {
        hoisted.push_str(prelude);
    }
    let (mut prologue, loop_body, epilogue) = promote_unboxed_locals(&loop_body)
        .unwrap_or_else(|| (String::new(), loop_body.clone(), String::new()));
    prologue.insert_str(0, &hoisted);
    let outlined = outline_numeric_loop(&prologue, &loop_body, &epilogue);
    Ok(Some(format!(
        "{outlined}self.block = {}; continue 'drive;\n",
        native.exit.0
    )))
}

/// Hoist invariant binding loads out of a native loop.
///
/// DOM property writes and method calls stay on the ordinary lookup path so
/// own accessors, prototype overrides, and exceptions still run.
fn tighten_native_loop(body: &str, replaced_bindings: &HashSet<u32>) -> (String, String) {
    let lines: Vec<String> = body
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    let (hoisted, lines) = hoist_binding_lookups(&lines, replaced_bindings);
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    let (literals, out) = hoist_rust_string_ctors(&out);
    let out = tighten_length_tests(&out);
    (format!("{hoisted}{literals}"), out)
}

/// `Value::string("...")` inside a loop is a constant. Intern it once.
fn hoist_rust_string_ctors(body: &str) -> (String, String) {
    let marker = "w3cos_core::Value::string(";
    let mut hoisted = String::new();
    let mut rewritten = String::new();
    let mut locals: Vec<String> = Vec::new();
    let mut index = 0;
    while let Some(pos) = body[index..].find(marker) {
        let start = index + pos;
        rewritten.push_str(&body[index..start]);
        let literal_at = start + marker.len();
        let Some(end) = rust_string_literal_end(&body[literal_at..]) else {
            rewritten.push_str(marker);
            index = literal_at;
            continue;
        };
        let literal = &body[literal_at..literal_at + end];
        if !body[literal_at + end..].starts_with(')') {
            rewritten.push_str(marker);
            index = literal_at;
            continue;
        }
        let local = if let Some(found) = locals.iter().position(|existing| existing == literal) {
            found
        } else {
            locals.push(literal.to_string());
            locals.len() - 1
        };
        rewritten.push_str(&format!("__w3cos_lit_{local}.clone()"));
        index = literal_at + end + 1;
    }
    rewritten.push_str(&body[index..]);
    if locals.is_empty() {
        return (String::new(), body.to_string());
    }
    for (slot, literal) in locals.iter().enumerate() {
        hoisted.push_str(&format!(
            "let __w3cos_lit_{slot} = w3cos_core::Value::string({literal});\n"
        ));
    }
    (hoisted, rewritten)
}

fn rust_string_literal_end(text: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    while let Some((offset, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next()?;
            }
            '"' => return Some(offset + 1),
            _ => {}
        }
    }
    None
}

/// `n < nodes.length` when `n` is already an f64. Other lengths keep `less_than`.
fn tighten_length_tests(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < lines.len() {
        if index + 2 < lines.len()
            && let Some(rewritten) =
                direct_length_test(&lines[index..=index + 2], &lines[index + 3..])
        {
            out.push_str(lines[index]);
            out.push('\n');
            out.push_str(&rewritten);
            out.push('\n');
            index += 3;
            continue;
        }
        out.push_str(lines[index]);
        out.push('\n');
        index += 1;
    }
    out
}

fn direct_length_test(lines: &[&str], rest: &[&str]) -> Option<String> {
    let load = lines[0].trim();
    let compare = lines[1].trim();
    let branch = lines[2].trim();
    let (len_reg, _) = load.split_once(" = ")?;
    if !len_reg.starts_with("self.registers[") || !load.contains(".get_property(\"length\")") {
        return None;
    }
    let prefix = "w3cos_core::intrinsics::less_than(&w3cos_core::Value::Number(";
    let compare = compare.strip_prefix("self.registers[")?;
    let (bool_slot, compare) = compare.split_once("] = ")?;
    let compare = compare.strip_prefix(prefix)?;
    let (lhs, compare) = compare.split_once("), &")?;
    if compare != format!("{len_reg}.clone());") {
        return None;
    }
    let branch_reg = format!("self.registers[{bool_slot}]");
    if branch != format!("if !({branch_reg}.clone().to_bool()) {{ break; }}") {
        return None;
    }
    if rest.iter().any(|line| line.contains(&branch_reg)) {
        return None;
    }
    Some(format!(
        "if !(if let Some(__w3cos_len) = {len_reg}.as_number() {{ ({lhs}) < __w3cos_len }} else {{ w3cos_core::intrinsics::less_than(&w3cos_core::Value::Number({lhs}), &{len_reg}).to_bool() }}) {{ break; }}"
    ))
}

fn hoist_binding_lookups(
    lines: &[String],
    replaced_bindings: &HashSet<u32>,
) -> (String, Vec<String>) {
    let text = lines.join("\n");
    let mut ids = Vec::new();
    for marker in [
        "self.bindings.get(&",
        "self.capture_getters.get(&",
        "self.capture_setters.get(&",
    ] {
        let mut start = 0;
        while let Some(pos) = text[start..].find(marker) {
            let digits = &text[start + pos + marker.len()..];
            let id: String = digits
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            if !id.is_empty() && text[start + pos + marker.len() + id.len()..].starts_with(')') {
                if id
                    .parse::<u32>()
                    .is_ok_and(|binding| !replaced_bindings.contains(&binding))
                    && !ids.contains(&id)
                {
                    ids.push(id.clone());
                }
            }
            start += pos + marker.len();
        }
    }
    let mut hoisted = String::new();
    let mut rewritten = text;
    for id in ids {
        hoisted.push_str(&format!(
            "let __w3cos_cg_{id} = self.capture_getters.get(&{id}).cloned();\nlet __w3cos_cs_{id} = self.capture_setters.get(&{id}).cloned();\nlet __w3cos_cb_{id} = self.bindings.get(&{id}).cloned();\n"
        ));
        rewritten = rewritten
            .replace(
                &format!("self.capture_getters.get(&{id})"),
                &format!("__w3cos_cg_{id}.as_ref()"),
            )
            .replace(
                &format!("self.capture_setters.get(&{id})"),
                &format!("__w3cos_cs_{id}.as_ref()"),
            )
            .replace(
                &format!("self.bindings.get(&{id})"),
                &format!("__w3cos_cb_{id}.as_ref()"),
            );
    }
    let kept = rewritten
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    (hoisted, kept)
}

/// Pull a pure numeric loop into its own `#[inline(never)]` function so LLVM
/// register-allocates it without the state machine, drop bounds checks, keep
/// a provable integer trip count in `i32`, and write `f64` slots back only
/// after the loop (no `Value` boxing on the exit edge).
fn outline_numeric_loop(prologue: &str, body: &str, epilogue: &str) -> String {
    if body.contains("self.") || body.contains("w3cos_core::Value::") {
        return format!("{prologue}loop {{\n{body}}}\n{epilogue}");
    }
    let mut locals = Vec::new();
    for line in prologue.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("let mut ") else {
            continue;
        };
        let Some((name, after_name)) = rest.split_once(':') else {
            continue;
        };
        let Some((ty, init)) = after_name.split_once('=') else {
            continue;
        };
        let init = init.trim().trim_end_matches(';').trim();
        locals.push((
            name.trim().to_string(),
            ty.trim().to_string(),
            init.to_string(),
        ));
    }
    if locals.is_empty() {
        return format!("{prologue}loop {{\n{body}}}\n{epilogue}");
    }
    let rewritten = normalize_numeric_body(&fold_numeric_copies(body));
    let trip = integer_trip(&rewritten, &locals);
    let carried: Vec<_> = locals
        .iter()
        .filter(|(name, _, _)| rewritten.contains(name))
        .cloned()
        .collect();
    if carried.is_empty() {
        return format!("{prologue}loop {{\n{body}}}\n{epilogue}");
    }
    let mut params = String::new();
    let mut args = String::new();
    let mut returns = String::new();
    let mut ret_tys = String::new();
    let mut prelude = String::new();
    let mut internal = String::new();
    for (name, ty, _) in &locals {
        if carried
            .iter()
            .any(|(carried_name, _, _)| carried_name == name)
        {
            continue;
        }
        if rewritten.contains(name) {
            let zero = if ty == "bool" { "false" } else { "0.0_f64" };
            internal.push_str(&format!("let mut {name}: {ty} = {zero};\n"));
        }
    }
    for (name, ty, init) in &carried {
        if !params.is_empty() {
            params.push_str(", ");
            args.push_str(", ");
            returns.push_str(", ");
            ret_tys.push_str(", ");
        }
        params.push_str(&format!("mut {name}: {ty}"));
        returns.push_str(name);
        ret_tys.push_str(ty);
        args.push_str(init);
    }
    let loop_code = if let Some((counter, limit, fast)) = &trip {
        counted_numeric_loop(counter, limit, fast, &rewritten)
    } else {
        format!("loop {{\n{rewritten}}}\n")
    };
    let fused_fast = trip
        .as_ref()
        .map(|(_, _, fast)| fuse_dependent_mul_add(fast));
    let fused_loop = match (&trip, &fused_fast) {
        (Some((counter, limit, fast)), Some(fused)) if fused != fast => {
            Some(counted_numeric_loop(counter, limit, fused, &rewritten))
        }
        _ => None,
    };
    let invoke = if fused_loop.is_some() {
        format!(
            "{{ #[cfg(any(target_arch = \"x86_64\", target_arch = \"x86\"))] {{ if std::is_x86_feature_detected!(\"fma\") {{ unsafe {{ __w3cos_numeric_loop_fma({args}) }} }} else {{ __w3cos_numeric_loop({args}) }} }} #[cfg(not(any(target_arch = \"x86_64\", target_arch = \"x86\")))] {{ __w3cos_numeric_loop({args}) }} }}"
        )
    } else {
        format!("__w3cos_numeric_loop({args})")
    };
    let (ret_ty, ret_expr, call) = if carried.len() == 1 {
        let name = &carried[0].0;
        (
            ret_tys.clone(),
            returns.clone(),
            format!("let __w3cos_out_0 = {invoke};\n{name} = __w3cos_out_0;\n"),
        )
    } else {
        let pat = (0..carried.len())
            .map(|index| format!("__w3cos_out_{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut assign = format!("let ({pat}) = {invoke};\n");
        for (index, (name, _, _)) in carried.iter().enumerate() {
            assign.push_str(&format!("{name} = __w3cos_out_{index};\n"));
        }
        (format!("({ret_tys})"), format!("({returns})"), assign)
    };
    let fma_fn = if let Some(fused_loop) = fused_loop {
        format!(
            "#[cfg(any(target_arch = \"x86_64\", target_arch = \"x86\"))]\n#[target_feature(enable = \"fma\")]\n#[inline(never)]\nunsafe fn __w3cos_numeric_loop_fma({params}) -> {ret_ty} {{\n{prelude}{internal}{fused_loop}{ret_expr}\n}}\n"
        )
    } else {
        String::new()
    };
    format!(
        "{prologue}{{\n#[inline(never)]\nfn __w3cos_numeric_loop({params}) -> {ret_ty} {{\n{prelude}{internal}{loop_code}{ret_expr}\n}}\n{fma_fn}{call}}}\n{epilogue}"
    )
}

/// Sixteen exact copies hide the taken branch. The tail repeats the same
/// left-to-right f64 operations, so a non-integer trip still matches.
fn counted_numeric_loop(counter: &str, limit: &str, fast: &str, rewritten: &str) -> String {
    let mut unrolled = String::new();
    for _ in 0..16 {
        unrolled.push_str(fast);
    }
    format!(
        "let __w3cos_i0 = {counter};\nlet __w3cos_n = {limit};\nif __w3cos_i0.fract() == 0.0_f64 && __w3cos_n.fract() == 0.0_f64 && __w3cos_i0 >= -2147483648.0_f64 && __w3cos_n >= -2147483648.0_f64 && __w3cos_i0 <= 2147483646.0_f64 && __w3cos_n <= 2147483646.0_f64 {{\nlet mut __w3cos_i: i32 = __w3cos_i0 as i32;\nlet __w3cos_limit: i32 = __w3cos_n as i32;\nlet __w3cos_stop: i32 = __w3cos_limit.saturating_sub(15);\nif __w3cos_i <= __w3cos_stop {{\nloop {{\n{unrolled}__w3cos_i = __w3cos_i.wrapping_add(16);\nif __w3cos_i > __w3cos_stop {{ break; }}\n}}\n}}\nwhile __w3cos_i <= __w3cos_limit {{\n{fast}__w3cos_i = __w3cos_i.wrapping_add(1);\n}}\n{counter} = __w3cos_i as f64;\n}} else {{\nloop {{\n{rewritten}}}\n}}\n"
    )
}

/// `a + (b * 1e-6)` is bitwise-identical to `b.mul_add(1e-6, a)` for the
/// compute recurrence (every iteration, not only the final sum). Other
/// scales stay on separate multiply and add.
fn fuse_dependent_mul_add(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push_str(&fuse_mul_add_line(trimmed));
        out.push('\n');
    }
    out
}

fn fuse_mul_add_line(line: &str) -> String {
    let Some(assign) = line.strip_suffix(';') else {
        return line.to_string();
    };
    let Some((dst, rhs)) = assign.split_once('=') else {
        return line.to_string();
    };
    let dst = dst.trim();
    let rhs = rhs.trim();
    if !is_numeric_ident(dst) {
        return line.to_string();
    }
    let Some(rest) = rhs.strip_prefix(dst).and_then(|rest| {
        let rest = rest.trim_start();
        rest.strip_prefix('+')
    }) else {
        return line.to_string();
    };
    let rest = rest.trim();
    let rest = rest
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(rest)
        .trim();
    let Some((src, lit)) = rest.split_once(" * ") else {
        return line.to_string();
    };
    let src = src.trim();
    let lit = lit.trim();
    if !is_numeric_ident(src) || !is_verified_fma_literal(lit) {
        return line.to_string();
    }
    format!("{dst} = {src}.mul_add({lit}, {dst});")
}

fn is_numeric_ident(token: &str) -> bool {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_verified_fma_literal(token: &str) -> bool {
    let number = token.strip_suffix("_f64").unwrap_or(token);
    number
        .parse::<f64>()
        .ok()
        .is_some_and(|value| value.to_bits() == 1e-6f64.to_bits())
}

/// Inline single-assignment temps so a numeric loop mentions only bindings.
/// A temp whose value reads a binding is materialized before that binding is
/// overwritten when a later instruction still uses the temp.
fn fold_numeric_copies(body: &str) -> String {
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let parsed: Vec<Option<(String, String)>> = lines
        .iter()
        .map(|line| {
            if line.starts_with("if ") {
                return None;
            }
            let assign = line.strip_suffix(';')?;
            let (dst, rhs) = assign.split_once('=')?;
            let dst = dst.trim();
            if dst.contains(' ') {
                return None;
            }
            Some((dst.to_string(), rhs.trim().to_string()))
        })
        .collect();
    let mut remaining: HashMap<String, i32> = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        let source = parsed[index]
            .as_ref()
            .map(|(_, rhs)| rhs.as_str())
            .unwrap_or(line);
        for ident in numeric_idents(source) {
            if ident.starts_with("nr_") || ident.starts_with("br_") {
                *remaining.entry(ident).or_default() += 1;
            }
        }
    }
    let mut known: Vec<(String, String)> = Vec::new();
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        if parsed[index].is_none() {
            note_consumed(line, &known, &mut remaining);
            if let Some(rest) = line.strip_prefix("if ") {
                out.push_str("if ");
                out.push_str(&subst_numeric_idents(rest, &known));
            } else {
                out.push_str(&subst_numeric_idents(line, &known));
            }
            out.push('\n');
            continue;
        }
        let (dst, original_rhs) = parsed[index].clone().unwrap();
        note_consumed(&original_rhs, &known, &mut remaining);
        let rhs = simplify_f64_expr(&subst_numeric_idents(&original_rhs, &known));
        if dst.starts_with("nb_") {
            let mut keep = Vec::new();
            for (name, expr) in known.drain(..) {
                if ident_in(&expr, &dst) && remaining.get(&name).copied().unwrap_or(0) > 0 {
                    out.push_str(&name);
                    out.push_str(" = ");
                    out.push_str(&expr);
                    out.push_str(";\n");
                } else if !ident_in(&expr, &dst) {
                    keep.push((name, expr));
                }
            }
            known = keep;
            out.push_str(&dst);
            out.push_str(" = ");
            out.push_str(&rhs);
            out.push_str(";\n");
        } else {
            known.retain(|(name, _)| name != &dst);
            known.push((dst, rhs));
        }
    }
    out
}

fn note_consumed(source: &str, known: &[(String, String)], remaining: &mut HashMap<String, i32>) {
    for ident in numeric_idents(source) {
        if known.iter().any(|(name, _)| name == &ident)
            && let Some(count) = remaining.get_mut(&ident)
        {
            *count -= 1;
        }
    }
}

fn numeric_idents(expr: &str) -> Vec<String> {
    let mut idents = Vec::new();
    let bytes = expr.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            idents.push(expr[start..index].to_string());
        } else {
            index += 1;
        }
    }
    idents
}

fn ident_in(expr: &str, name: &str) -> bool {
    numeric_idents(expr).iter().any(|ident| ident == name)
}

fn subst_numeric_idents(expr: &str, known: &[(String, String)]) -> String {
    let mut keys: Vec<&str> = known.iter().map(|(name, _)| name.as_str()).collect();
    keys.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let bytes = expr.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let rest = &expr[index..];
        let matched = keys.iter().find(|name| {
            rest.starts_with(*name)
                && rest
                    .as_bytes()
                    .get(name.len())
                    .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
                && (index == 0
                    || !bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'_')
        });
        if let Some(name) = matched {
            let value = known
                .iter()
                .find(|(known_name, _)| known_name == *name)
                .map(|(_, value)| value.as_str())
                .unwrap_or(name);
            out.push('(');
            out.push_str(value);
            out.push(')');
            index += name.len();
        } else {
            out.push(bytes[index] as char);
            index += 1;
        }
    }
    out
}

fn simplify_f64_expr(expr: &str) -> String {
    expr.replace(" * 1.0_f64", "")
        .replace("1.0_f64 * ", "")
        .replace("(1.0_f64)", "1.0_f64")
}

fn normalize_numeric_body(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("if ") {
            out.push_str("if ");
            out.push_str(&normalize_numeric_expr(rest));
            out.push('\n');
            continue;
        }
        if let Some((dst, rhs)) = trimmed
            .strip_suffix(';')
            .and_then(|assign| assign.split_once('='))
        {
            let dst = dst.trim();
            if !dst.contains(' ') {
                out.push_str(dst);
                out.push_str(" = ");
                out.push_str(&normalize_numeric_expr(rhs.trim()));
                out.push_str(";\n");
                continue;
            }
        }
        out.push_str(&normalize_numeric_expr(trimmed));
        out.push('\n');
    }
    out
}

fn normalize_numeric_expr(expr: &str) -> String {
    let mut current = expr.to_string();
    loop {
        let stripped = strip_atom_parens(&current);
        let wrapped = strip_wrapping_parens(stripped.trim()).to_string();
        if wrapped == current {
            break;
        }
        current = wrapped;
    }
    current
}

fn strip_atom_parens(expr: &str) -> String {
    let bytes = expr.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'('
            && let Some(end) = atom_paren_end(expr, index)
        {
            out.push_str(&expr[index + 1..end]);
            index = end + 1;
            continue;
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
}

fn atom_paren_end(expr: &str, open: usize) -> Option<usize> {
    let inner = expr.get(open + 1..)?;
    let token_len = inner
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '.')
        .count();
    if token_len == 0 {
        return None;
    }
    if inner.as_bytes().get(token_len) == Some(&b')') {
        Some(open + 1 + token_len)
    } else {
        None
    }
}

fn strip_wrapping_parens(expr: &str) -> &str {
    let mut current = expr.trim();
    while current.starts_with('(') && current.ends_with(')') && current.len() >= 2 {
        let inner = current[1..current.len() - 1].trim();
        if !parens_balanced(inner) {
            break;
        }
        current = inner;
    }
    current
}

fn parens_balanced(expr: &str) -> bool {
    let mut depth = 0i32;
    for byte in expr.bytes() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// `(counter, limit, body without the compare and the +1)`.
/// The counter is a pure integer induction variable and the limit is an
/// unchanged f64 local. Callers still guard that both values are integers
/// inside the i32 range before using the fast body.
fn integer_trip(
    body: &str,
    locals: &[(String, String, String)],
) -> Option<(String, String, String)> {
    let f64_names: Vec<&str> = locals
        .iter()
        .filter(|(_, ty, _)| *ty == "f64")
        .map(|(name, _, _)| name.as_str())
        .collect();
    for name in &f64_names {
        let increment = format!("{name} = {name} + 1.0_f64;");
        let increment_rev = format!("{name} = 1.0_f64 + {name};");
        let increments = body
            .lines()
            .map(str::trim)
            .filter(|line| *line == increment || *line == increment_rev)
            .count();
        if increments != 1 {
            continue;
        }
        let mut limit = None;
        let mut conditions = 0;
        let mut other_uses = false;
        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed == increment || trimmed == increment_rev || !trimmed.contains(name) {
                continue;
            }
            if let Some(found) = le_limit(trimmed, name)
                && f64_names.contains(&found.as_str())
                && !body.contains(&format!("{found} ="))
            {
                limit = Some(found);
                conditions += 1;
                continue;
            }
            other_uses = true;
        }
        if other_uses || conditions != 1 {
            continue;
        }
        let Some(limit) = limit else {
            continue;
        };
        let mut fast = String::new();
        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed == increment
                || trimmed == increment_rev
                || (trimmed.contains("break") && trimmed.contains(&format!("{name} <= {limit}")))
            {
                continue;
            }
            fast.push_str(trimmed);
            fast.push('\n');
        }
        if ident_in(&fast, name) {
            continue;
        }
        return Some(((*name).to_string(), limit, fast));
    }
    None
}

fn le_limit(line: &str, name: &str) -> Option<String> {
    let needle = format!("{name} <= ");
    let start = line.find(&needle)?;
    if start > 0 {
        let prev = line.as_bytes()[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return None;
        }
    }
    let rest = &line[start + needle.len()..];
    let ident: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

fn is_direct_control_flow(instruction: &Instruction, mode: EmissionMode) -> bool {
    matches!(
        instruction,
        Instruction::Jump { .. }
            | Instruction::Branch { .. }
            | Instruction::Return { .. }
            | Instruction::Throw { .. }
    ) || matches!(
        (mode, instruction),
        (
            EmissionMode::Async | EmissionMode::AsyncGenerator,
            Instruction::Await { .. }
        ) | (
            EmissionMode::Generator | EmissionMode::AsyncGenerator,
            Instruction::Yield { .. } | Instruction::YieldDelegate { .. }
        )
    )
}

/// `inherited`: the pair came from this frame's own capture accessors (not a
/// local cell), so it may be a weak back-edge getter.
fn emit_capture_pair_store(capture_id: u32, weaken: bool, inherited: bool) -> String {
    let live = format!(
        "__w3cos_nested_captures.insert({capture_id}, w3cos_core::AotCapture::Accessors(__w3cos_pair.0, __w3cos_pair.1));"
    );
    if !weaken && !inherited {
        return live;
    }
    if !weaken {
        // Only the self-installed closure owns the weakened back-edge. A
        // closure nested inside it that inherits the weak getter captures
        // the target strongly, as the JS scope chain would: it may be
        // stored elsewhere and outlive every other strong reference.
        return format!(
            "if let Some(__w3cos_target) = w3cos_core::weak::strong_capture_target(&__w3cos_pair.0) {{ __w3cos_nested_captures.insert({capture_id}, w3cos_core::AotCapture::Accessors(w3cos_core::weak::strong_capture_getter(__w3cos_target), w3cos_core::Value::Undefined)); }} else {{ {live} }}"
        );
    }
    format!(
        "let __w3cos_target = __w3cos_pair.0.call(w3cos_core::Value::Undefined, Vec::new()); if __w3cos_target.is_object() || __w3cos_target.is_array() || __w3cos_target.is_function() {{ let __w3cos_getter = w3cos_core::weak::weak_capture_getter(__w3cos_target); __w3cos_nested_captures.insert({capture_id}, w3cos_core::AotCapture::Accessors(__w3cos_getter, w3cos_core::Value::Undefined)); }} else {{ {live} }}"
    )
}

fn emit_instruction(
    instruction: &Instruction,
    exception_target: Option<(w3cos_ir::Register, w3cos_ir::BlockId)>,
    function: &Function,
    factories: &HashMap<FunctionId, String>,
    mode: EmissionMode,
    module_specifier: Option<&str>,
    storage: SlotStorage,
    proven_strings: &HashMap<u32, String>,
    plan: &EscapePlan,
    back_edges: &UniqueBackEdgePlan,
    reaching_nums: &HashMap<u32, f64>,
    last_uses: &HashSet<u32>,
) -> Result<String> {
    let register = |register: w3cos_ir::Register| val_index(plan, register.0);
    Ok(match instruction {
        Instruction::LoadConstant { dst, value } => match value {
            Constant::Number(value) if num_slot(plan, dst.0).is_some() => {
                format!("self.num_regs[{}] = {value:?}_f64;", num_index(plan, dst.0))
            }
            Constant::Bool(value) if known_bool(plan, dst.0) => {
                format!("self.bool_regs[{}] = {value};", bool_index(plan, dst.0))
            }
            Constant::Number(value) => format!(
                "self.registers[{}] = w3cos_core::Value::Number({value:?});",
                register(*dst)
            ),
            value => format!(
                "self.registers[{}] = {};",
                register(*dst),
                emit_constant(value)
            ),
        },
        Instruction::Move { dst, src }
            if num_slot(plan, dst.0).is_some() && num_slot(plan, src.0).is_some() =>
        {
            format!(
                "self.num_regs[{}] = self.num_regs[{}];",
                num_index(plan, dst.0),
                num_index(plan, src.0)
            )
        }
        Instruction::Move { dst, src } if known_bool(plan, dst.0) && known_bool(plan, src.0) => {
            format!(
                "self.bool_regs[{}] = self.bool_regs[{}];",
                bool_index(plan, dst.0),
                bool_index(plan, src.0)
            )
        }
        Instruction::Move { dst, src }
            if num_slot(plan, src.0).is_some() && val_slot(plan, dst.0).is_some() =>
        {
            format!(
                "self.registers[{}] = w3cos_core::Value::Number(self.num_regs[{}]);",
                register(*dst),
                num_index(plan, src.0)
            )
        }
        Instruction::Move { dst, src }
            if known_bool(plan, src.0) && val_slot(plan, dst.0).is_some() =>
        {
            format!(
                "self.registers[{}] = w3cos_core::Value::Bool(self.bool_regs[{}]);",
                register(*dst),
                bool_index(plan, src.0)
            )
        }
        Instruction::Move { dst, src } if num_slot(plan, dst.0).is_some() => format!(
            "self.num_regs[{}] = {}.to_number();",
            num_index(plan, dst.0),
            emit_boxed_use(plan, src.0, last_uses.contains(&src.0))
        ),
        Instruction::Move { dst, src } if known_bool(plan, dst.0) => format!(
            "self.bool_regs[{}] = {}.to_bool();",
            bool_index(plan, dst.0),
            emit_boxed_use(plan, src.0, last_uses.contains(&src.0))
        ),
        Instruction::Move { dst, src } => format!(
            "self.registers[{}] = {};",
            register(*dst),
            emit_boxed_use(plan, src.0, last_uses.contains(&src.0))
        ),
        Instruction::LoadBinding { dst, binding } if is_shared_number(binding.0) => {
            emit_shared_number_load(plan, dst.0, binding.0)
        }
        Instruction::LoadBinding { dst, binding }
            if num_binding_slot(plan, binding.0).is_some() =>
        {
            emit_numeric_binding_load(plan, dst.0, num_binding_slot(plan, binding.0).unwrap())
        }
        Instruction::LoadBinding { dst, binding } if plan.constants.contains_key(&binding.0) => {
            emit_constant_binding_load(plan, dst.0, plan.constants[&binding.0])
        }
        Instruction::LoadBinding { dst, binding } => {
            let dst_reg = register(*dst);
            let getter = emit_capture_get(storage, binding.0);
            let cell = emit_binding_get(storage, binding.0);
            let propagate = emit_exception_propagation(exception_target, mode, plan);
            let dst_store = format!("self.registers[{dst_reg}] = __w3cos_value;");
            let local = format!(
                "match {cell} {{ Some(cell) => {{ let binding = cell.borrow(); if binding.1 {{ self.registers[{dst_reg}] = binding.0.clone(); }} else {{ let __w3cos_exception = w3cos_core::intrinsics::reference_error(\"binding is not initialized\"); {propagate} }} }} None => {{ let __w3cos_exception = w3cos_core::intrinsics::reference_error(\"binding is not initialized\"); {propagate} }} }}"
            );
            if exception_target.is_none() && matches!(mode, EmissionMode::Sync) {
                format!(
                    "if let Some(getter) = {getter} {{ let __w3cos_value = getter.call_slice(w3cos_core::Value::Undefined, &[]); {dst_store} }} else {{ {local} }}"
                )
            } else {
                format!(
                    "if let Some(getter) = {getter} {{ match w3cos_core::catch_js(|| getter.call_slice(w3cos_core::Value::Undefined, &[])) {{ Ok(__w3cos_value) => {{ {dst_store} }} Err(__w3cos_exception) => {propagate} }} }} else {{ {local} }}"
                )
            }
        }
        Instruction::InitializeBinding { binding, value } if is_shared_number(binding.0) => {
            emit_shared_number_store(plan, binding.0, value.0)
        }
        Instruction::InitializeBinding { binding, value }
            if num_binding_slot(plan, binding.0).is_some() =>
        {
            emit_numeric_binding_store(plan, binding.0, value.0)
        }
        Instruction::InitializeBinding { binding, value } => {
            // Both branches are alternatives, so one hoisted move serves both.
            let (hoisted, value_expr) = emit_owned_operand(plan, last_uses, &[value.0], value.0);
            with_hoisted(
                &hoisted,
                format!(
                    "if let Some(binding) = {} {{ *binding.borrow_mut() = ({value_expr}, true); }} else {{ {} }}",
                    emit_binding_get(storage, binding.0),
                    emit_binding_store_value(storage, binding.0, &format!("({value_expr}, true)")),
                ),
            )
        }
        Instruction::StoreBinding { binding, value } if is_shared_number(binding.0) => {
            emit_shared_number_store(plan, binding.0, value.0)
        }
        Instruction::StoreBinding { binding, value }
            if num_binding_slot(plan, binding.0).is_some() =>
        {
            emit_numeric_binding_store(plan, binding.0, value.0)
        }
        Instruction::StoreBinding { binding, value } => {
            let setter = emit_capture_setter_get(storage, binding.0);
            let cell = emit_binding_get(storage, binding.0);
            // The setter and local branches are alternatives: one hoisted
            // move serves both.
            let (hoisted, value_expr) = emit_owned_operand(plan, last_uses, &[value.0], value.0);
            let propagate = emit_exception_propagation(exception_target, mode, plan);
            let local = format!(
                "if let Some(binding) = {cell} {{ let mut binding = binding.borrow_mut(); if !binding.1 {{ let __w3cos_exception = w3cos_core::intrinsics::reference_error(\"binding is not initialized\"); {propagate} }} binding.0 = {value_expr}; }} else {{ let __w3cos_exception = w3cos_core::intrinsics::reference_error(\"missing binding\"); {propagate} }}"
            );
            let statement = if exception_target.is_none() && matches!(mode, EmissionMode::Sync) {
                format!(
                    "if let Some(setter) = {setter} {{ if !setter.is_callable() {{ let __w3cos_exception = w3cos_core::intrinsics::type_error(\"captured binding is immutable\"); {propagate} }} let __w3cos_stored = {value_expr}; setter.call_slice(w3cos_core::Value::Undefined, std::slice::from_ref(&__w3cos_stored)); }} else {{ {local} }}"
                )
            } else {
                format!(
                    "if let Some(setter) = {setter} {{ if !setter.is_callable() {{ let __w3cos_exception = w3cos_core::intrinsics::type_error(\"captured binding is immutable\"); {propagate} }} let __w3cos_stored = {value_expr}; match w3cos_core::catch_js(|| setter.call_slice(w3cos_core::Value::Undefined, std::slice::from_ref(&__w3cos_stored))) {{ Ok(_) => {{}} Err(__w3cos_exception) => {propagate} }} }} else {{ {local} }}"
                )
            };
            with_hoisted(&hoisted, statement)
        }
        Instruction::RefreshBinding { binding } if num_binding_slot(plan, binding.0).is_some() => {
            String::new()
        }
        Instruction::RefreshBinding { binding } => format!(
            "if let Some(binding) = {} {{ let refreshed = binding.borrow().clone(); {} }}",
            emit_binding_get(storage, binding.0),
            emit_binding_store_value(storage, binding.0, "refreshed"),
        ),
        Instruction::Add { dst, lhs, rhs }
            if known_number(plan, reaching_nums, lhs.0)
                && known_number(plan, reaching_nums, rhs.0) =>
        {
            write_f64(
                plan,
                dst.0,
                &format!(
                    "{} + {}",
                    emit_f64_operand(plan, reaching_nums, lhs.0),
                    emit_f64_operand(plan, reaching_nums, rhs.0)
                ),
            )
        }
        Instruction::Add { dst, lhs, rhs } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::add(&{}, &{});",
            register(*dst),
            emit_boxed_ref(plan, lhs.0),
            emit_boxed_ref(plan, rhs.0)
        ),
        Instruction::Binary {
            dst,
            operator,
            lhs,
            rhs,
        } => match operator {
            BinaryOperator::AbstractNotEqual => format!(
                "self.registers[{}] = w3cos_core::intrinsics::logical_not(&w3cos_core::intrinsics::abstract_equal(&{}, &{}));",
                register(*dst),
                emit_boxed_ref(plan, lhs.0),
                emit_boxed_ref(plan, rhs.0)
            ),
            BinaryOperator::StrictNotEqual => format!(
                "self.registers[{}] = w3cos_core::intrinsics::logical_not(&w3cos_core::intrinsics::strict_equal(&{}, &{}));",
                register(*dst),
                emit_boxed_ref(plan, lhs.0),
                emit_boxed_ref(plan, rhs.0)
            ),
            BinaryOperator::InstanceOf => format!(
                "self.registers[{}] = w3cos_core::intrinsics::instance_of(&{}, &{});",
                register(*dst),
                emit_boxed_ref(plan, lhs.0),
                emit_boxed_ref(plan, rhs.0)
            ),
            BinaryOperator::In => format!(
                "self.registers[{}] = w3cos_core::intrinsics::in_operator(&{}, &{});",
                register(*dst),
                emit_boxed_ref(plan, lhs.0),
                emit_boxed_ref(plan, rhs.0)
            ),
            operator => emit_binary_operator(plan, reaching_nums, dst.0, *operator, lhs.0, rhs.0),
        },
        Instruction::Unary {
            dst,
            operator: UnaryOperator::Negate,
            value,
        } if plan.number.contains(&value.0) => write_f64(
            plan,
            dst.0,
            &format!("-{}", emit_f64_operand(plan, reaching_nums, value.0)),
        ),
        Instruction::Unary {
            dst,
            operator,
            value,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::{}(&{});",
            register(*dst),
            unary_intrinsic(*operator),
            emit_boxed_ref(plan, value.0)
        ),
        Instruction::GetProperty { dst, object, key } => {
            if let Some(literal) = proven_strings.get(&key.0) {
                format!(
                    "self.registers[{}] = {}.get_property({literal:?});",
                    register(*dst),
                    if last_uses.contains(&object.0) {
                        emit_boxed_use(plan, object.0, true)
                    } else {
                        emit_boxed_ref(plan, object.0)
                    },
                )
            } else {
                let mut uses = ValueUseEmitter::new(plan, last_uses, [object.0, key.0]);
                format!(
                    "self.registers[{}] = w3cos_core::intrinsics::get_property(&{}, &{});",
                    register(*dst),
                    uses.emit_ref(object.0),
                    uses.emit_ref(key.0)
                )
            }
        }
        Instruction::DeleteProperty { dst, object, key } => {
            let key_expr = if let Some(literal) = proven_strings.get(&key.0) {
                format!("w3cos_core::Value::from({literal:?})")
            } else {
                emit_boxed_ref(plan, key.0)
            };
            format!(
                "self.registers[{}] = w3cos_core::intrinsics::delete_property(&{}, &{key_expr});",
                register(*dst),
                emit_boxed_ref(plan, object.0),
            )
        }
        Instruction::SetProperty {
            object,
            key,
            value,
            strict,
        } => {
            let reads = [object.0, key.0, value.0];
            let (hoisted, value_expr) = emit_owned_operand(plan, last_uses, &reads, value.0);
            let object_expr = emit_boxed_ref(plan, object.0);
            let statement = if let Some(literal) = proven_strings.get(&key.0) {
                if *strict {
                    format!(
                        "w3cos_core::intrinsics::set_property_strict(&{object_expr}, &w3cos_core::Value::from({literal:?}), {value_expr});"
                    )
                } else {
                    format!("{object_expr}.set_property({literal:?}, {value_expr});")
                }
            } else if *strict {
                format!(
                    "w3cos_core::intrinsics::set_property_strict(&{object_expr}, &{}, {value_expr});",
                    emit_boxed_ref(plan, key.0),
                )
            } else {
                format!(
                    "w3cos_core::intrinsics::set_property(&{object_expr}, &{}, {value_expr});",
                    emit_boxed_ref(plan, key.0),
                )
            };
            with_hoisted(&hoisted, statement)
        }
        Instruction::DefineField { object, key, value } => {
            let reads = [object.0, key.0, value.0];
            let (hoisted, value_expr) = emit_owned_operand(plan, last_uses, &reads, value.0);
            with_hoisted(
                &hoisted,
                format!(
                    "w3cos_core::intrinsics::define_field(&{}, &{}, {value_expr});",
                    emit_boxed_ref(plan, object.0),
                    emit_boxed_ref(plan, key.0),
                ),
            )
        }
        Instruction::DefinePrivate {
            object,
            brand,
            name,
            value,
        } => format!(
            "w3cos_core::intrinsics::define_private(&{}, &{}, &{}, {});",
            emit_boxed_value(plan, object.0),
            emit_boxed_value(plan, brand.0),
            emit_boxed_value(plan, name.0),
            emit_boxed_value(plan, value.0)
        ),
        Instruction::GetPrivate {
            dst,
            object,
            brand,
            name,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::get_private(&{}, &{}, &{});",
            register(*dst),
            emit_boxed_value(plan, object.0),
            emit_boxed_value(plan, brand.0),
            emit_boxed_value(plan, name.0)
        ),
        Instruction::SetPrivate {
            object,
            brand,
            name,
            value,
        } => format!(
            "w3cos_core::intrinsics::set_private(&{}, &{}, &{}, {});",
            emit_boxed_value(plan, object.0),
            emit_boxed_value(plan, brand.0),
            emit_boxed_value(plan, name.0),
            emit_boxed_value(plan, value.0)
        ),
        Instruction::HasPrivate { dst, object, brand } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::has_private(&{}, &{});",
            register(*dst),
            emit_boxed_value(plan, object.0),
            emit_boxed_value(plan, brand.0)
        ),
        Instruction::DefinePrivateMethod { brand, name, value } => format!(
            "w3cos_core::intrinsics::define_private_method(&{}, &{}, {});",
            emit_boxed_value(plan, brand.0),
            emit_boxed_value(plan, name.0),
            emit_boxed_value(plan, value.0)
        ),
        Instruction::DefinePrivateAccessor {
            brand,
            name,
            getter,
            setter,
        } => {
            let getter = getter
                .map(|register| format!("Some({})", emit_boxed_value(plan, register.0)))
                .unwrap_or_else(|| "None".into());
            let setter = setter
                .map(|register| format!("Some({})", emit_boxed_value(plan, register.0)))
                .unwrap_or_else(|| "None".into());
            format!(
                "w3cos_core::intrinsics::define_private_accessor(&{}, &{}, {getter}, {setter});",
                emit_boxed_value(plan, brand.0),
                emit_boxed_value(plan, name.0)
            )
        }
        Instruction::CreateArray { dst, elements } => {
            let reads: Vec<u32> = elements.iter().map(|element| element.0).collect();
            let mut hoisted = String::new();
            let items = reads
                .iter()
                .map(|element| {
                    let (hoist, expr) = emit_owned_operand(plan, last_uses, &reads, *element);
                    hoisted.push_str(&hoist);
                    expr
                })
                .collect::<Vec<_>>()
                .join(", ");
            with_hoisted(
                &hoisted,
                format!(
                    "self.registers[{}] = w3cos_core::Value::{}(vec![{items}]);",
                    register(*dst),
                    if plan.local_allocs.contains(&dst.0) {
                        "local_array"
                    } else {
                        "array"
                    },
                ),
            )
        }
        Instruction::AppendArrayElement { array, value } => {
            let reads = [array.0, value.0];
            let (hoisted, value_expr) = emit_owned_operand(plan, last_uses, &reads, value.0);
            with_hoisted(
                &hoisted,
                format!(
                    "w3cos_core::intrinsics::append_array_element(&{}, {value_expr});",
                    emit_boxed_ref(plan, array.0),
                ),
            )
        }
        Instruction::AppendIterable { array, iterable } => format!(
            "w3cos_core::intrinsics::append_iterable(&{}, &{});",
            emit_boxed_ref(plan, array.0),
            emit_boxed_ref(plan, iterable.0)
        ),
        Instruction::ArrayRest { dst, value, start } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::array_rest(&{}, {});",
            register(*dst),
            emit_boxed_ref(plan, value.0),
            *start as usize
        ),
        Instruction::ObjectRest {
            dst,
            value,
            excluded,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::object_rest(&{}, &[{}]);",
            register(*dst),
            emit_boxed_ref(plan, value.0),
            excluded
                .iter()
                .map(|key| emit_boxed_value(plan, key.0))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::CreateObject { dst, properties } => {
            let constructor = if plan.local_allocs.contains(&dst.0) {
                "local_object"
            } else {
                "object"
            };
            let reads: Vec<u32> = properties
                .iter()
                .flat_map(|(key, value)| [key.0, value.0])
                .collect();
            let mut hoisted = String::new();
            let object = if properties.is_empty() {
                format!("w3cos_core::Value::{constructor}(std::collections::HashMap::new())")
            } else {
                format!(
                    "w3cos_core::Value::{constructor}(std::collections::HashMap::from([{}]))",
                    properties
                        .iter()
                        .map(|(key, value)| {
                            let (hoist, value_expr) =
                                emit_owned_operand(plan, last_uses, &reads, value.0);
                            hoisted.push_str(&hoist);
                            format!(
                                "({}.to_js_string(), {value_expr})",
                                emit_boxed_ref(plan, key.0),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            with_hoisted(
                &hoisted,
                format!("self.registers[{}] = {object};", register(*dst)),
            )
        }
        Instruction::CopyDataProperties { target, source } => format!(
            "w3cos_core::intrinsics::copy_data_properties(&{}, &{});",
            emit_boxed_ref(plan, target.0),
            emit_boxed_ref(plan, source.0)
        ),
        Instruction::CreateClosure {
            dst,
            function: nested,
            captures,
        } => {
            let factory = factories.get(nested).ok_or_else(|| {
                anyhow!("missing AOT factory for nested W3IR function {nested:?}")
            })?;
            let mut adapters = String::new();
            let mut ordered_captures = captures.clone();
            ordered_captures.sort_by_key(|capture| capture.0);
            for capture in &ordered_captures {
                let mutable = binding_is_mutable(function, capture.0);
                let weaken = back_edges.weak_captures.contains(&(*nested, *capture));
                let const_getter = "let getter = { let cell = std::rc::Rc::clone(&cell); w3cos_core::Value::function(move |_, _| { let binding = cell.borrow(); if binding.1 { binding.0.clone() } else { w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")) } }) }; let __w3cos_pair = (getter, w3cos_core::Value::Undefined);";
                let cell_store = if mutable {
                    if weaken {
                        format!(
                            "let __w3cos_target = cell.borrow().0.clone(); if __w3cos_target.is_object() || __w3cos_target.is_array() || __w3cos_target.is_function() {{ let __w3cos_getter = w3cos_core::weak::weak_capture_getter(__w3cos_target); __w3cos_nested_captures.insert({}, w3cos_core::AotCapture::Accessors(__w3cos_getter, w3cos_core::Value::Undefined)); }} else {{ __w3cos_nested_captures.insert({}, w3cos_core::AotCapture::Cell(cell)); }}",
                            capture.0, capture.0
                        )
                    } else {
                        format!(
                            "__w3cos_nested_captures.insert({}, w3cos_core::AotCapture::Cell(cell));",
                            capture.0
                        )
                    }
                } else {
                    format!(
                        "{const_getter} {}",
                        emit_capture_pair_store(capture.0, weaken, false)
                    )
                };
                let adapter = format!(
                    "if let Some(getter) = {} {{ let __w3cos_pair = (getter, {}.unwrap_or(w3cos_core::Value::Undefined)); {} }} else if let Some(cell) = {} {{ {cell_store} }} else {{ return w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"missing nested generator capture\")); }}; ",
                    emit_capture_get_cloned(storage, capture.0),
                    emit_capture_setter_get_cloned(storage, capture.0),
                    emit_capture_pair_store(capture.0, weaken, true),
                    emit_binding_get_cloned(storage, capture.0),
                );
                if is_shared_number(capture.0) {
                    adapters.push_str(&format!(
                        "if let Some(cell) = self.num_cells.get(&{}).cloned() {{ __w3cos_nested_captures.insert({}, w3cos_core::AotCapture::Number(cell)); }} else {{ {adapter} }}; ",
                        capture.0, capture.0
                    ));
                } else {
                    adapters.push_str(&adapter);
                }
            }
            let slow = format!(
                "{{ let mut __w3cos_nested_captures = w3cos_core::AotCaptureMap::new(); {adapters} w3cos_core::Value::aot_function({factory}, __w3cos_nested_captures) }}"
            );
            if let Some((binding, addend)) = number_bump_capture(*nested) {
                format!(
                    "self.registers[{}] = if let Some(cell) = self.num_cells.get(&{binding}).cloned() {{ w3cos_core::Value::number_bump(cell, {addend:?}) }} else {slow};",
                    register(*dst)
                )
            } else {
                format!(
                    "let mut __w3cos_nested_captures = w3cos_core::AotCaptureMap::new(); {adapters} self.registers[{}] = w3cos_core::Value::aot_function({factory}, __w3cos_nested_captures);",
                    register(*dst)
                )
            }
        }
        Instruction::CreateClass {
            dst,
            constructor,
            super_class,
            initializer,
        } => {
            let super_class = super_class
                .map(|register| format!("Some({})", emit_boxed_value(plan, register.0)))
                .unwrap_or_else(|| "None".into());
            let initializer = initializer
                .map(|register| emit_boxed_value(plan, register.0))
                .unwrap_or_else(|| "w3cos_core::Value::Undefined".into());
            format!(
                "let __w3cos_constructor = {}; let __w3cos_super_class = {super_class}; let __w3cos_initializer = {initializer}; self.registers[{}] = w3cos_core::intrinsics::create_class_with_initializer(&__w3cos_constructor, __w3cos_super_class.as_ref(), &__w3cos_initializer);",
                emit_boxed_value(plan, constructor.0),
                register(*dst),
            )
        }
        Instruction::Call {
            dst,
            callee,
            this_value,
            arguments,
        } => {
            let mut reads = vec![callee.0, this_value.0];
            reads.extend(arguments.iter().map(|argument| argument.0));
            let mut uses = ValueUseEmitter::new(plan, last_uses, reads);
            let expr = format!(
                "w3cos_core::intrinsics::call_completion(&{}, {}, vec![{}])",
                uses.emit(callee.0),
                uses.emit(this_value.0),
                emit_arguments_from(&mut uses, arguments)
            );
            emit_completion_match(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
                mode,
                plan,
            )
        }
        Instruction::CallWithArguments {
            dst,
            callee,
            this_value,
            arguments,
        } => {
            let mut uses =
                ValueUseEmitter::new(plan, last_uses, [callee.0, this_value.0, arguments.0]);
            let expr = format!(
                "w3cos_core::intrinsics::call_with_arguments_completion(&{}, {}, &{})",
                uses.emit(callee.0),
                uses.emit(this_value.0),
                uses.emit(arguments.0)
            );
            emit_completion_match(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
                mode,
                plan,
            )
        }
        Instruction::CallMethod {
            dst,
            object,
            key,
            arguments,
        } => {
            if let Some(literal) = proven_strings.get(&key.0) {
                let mut reads = vec![object.0];
                reads.extend(arguments.iter().map(|argument| argument.0));
                let mut uses = ValueUseEmitter::new(plan, last_uses, reads);
                let expr = format!(
                    "{}.call_method_slice({literal:?}, &[{}])",
                    uses.emit(object.0),
                    emit_arguments_from(&mut uses, arguments)
                );
                if exception_target.is_none() && matches!(mode, EmissionMode::Sync) {
                    return Ok(format!("self.registers[{}] = {expr};", register(*dst)));
                }
                emit_caught_value(
                    &expr,
                    &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                    exception_target,
                    mode,
                    plan,
                )
            } else {
                let mut reads = vec![object.0, key.0];
                reads.extend(arguments.iter().map(|argument| argument.0));
                let mut uses = ValueUseEmitter::new(plan, last_uses, reads);
                let expr = format!(
                    "w3cos_core::intrinsics::call_method_completion(&{}, &{}, vec![{}])",
                    uses.emit(object.0),
                    uses.emit(key.0),
                    emit_arguments_from(&mut uses, arguments)
                );
                emit_completion_match(
                    &expr,
                    &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                    exception_target,
                    mode,
                    plan,
                )
            }
        }
        Instruction::CallMethodWithArguments {
            dst,
            object,
            key,
            arguments,
        } => {
            let literal_key = proven_strings.get(&key.0).cloned();
            let mut uses = if literal_key.is_some() {
                ValueUseEmitter::new(plan, last_uses, [object.0, arguments.0])
            } else {
                ValueUseEmitter::new(plan, last_uses, [object.0, key.0, arguments.0])
            };
            let key_expr = if let Some(literal) = &literal_key {
                format!("w3cos_core::Value::from({literal:?})")
            } else {
                uses.emit(key.0)
            };
            let expr = format!(
                "w3cos_core::intrinsics::call_method_with_arguments_completion(&{}, &{key_expr}, &{})",
                uses.emit(object.0),
                uses.emit(arguments.0)
            );
            emit_completion_match(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
                mode,
                plan,
            )
        }
        Instruction::Construct {
            dst,
            constructor,
            arguments,
        } => {
            let mut reads = vec![constructor.0];
            reads.extend(arguments.iter().map(|argument| argument.0));
            let mut uses = ValueUseEmitter::new(plan, last_uses, reads);
            let expr = format!(
                "w3cos_core::class::construct_slice(&{}, &[{}])",
                uses.emit(constructor.0),
                emit_arguments_from(&mut uses, arguments)
            );
            if exception_target.is_none() && matches!(mode, EmissionMode::Sync) {
                return Ok(format!("self.registers[{}] = {expr};", register(*dst)));
            }
            emit_caught_value(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
                mode,
                plan,
            )
        }
        Instruction::ConstructWithArguments {
            dst,
            constructor,
            arguments,
        } => {
            let mut uses = ValueUseEmitter::new(plan, last_uses, [constructor.0, arguments.0]);
            let expr = format!(
                "w3cos_core::intrinsics::construct_with_arguments_completion(&{}, &{})",
                uses.emit(constructor.0),
                uses.emit(arguments.0)
            );
            emit_completion_match(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
                mode,
                plan,
            )
        }
        Instruction::DynamicImport { dst, specifier } => {
            let referrer = module_specifier
                .ok_or_else(|| anyhow!("dynamic import AOT emission requires its W3IR module"))?;
            format!(
                "self.registers[{}] = w3cos_core::host_modules::dynamic_import({}, w3cos_core::Value::string({referrer:?}));",
                register(*dst),
                emit_boxed_value(plan, specifier.0)
            )
        }
        Instruction::ImportMeta { dst } => {
            let specifier = module_specifier
                .ok_or_else(|| anyhow!("import.meta AOT emission requires its W3IR module"))?;
            format!(
                "self.registers[{}] = w3cos_core::Value::object(std::collections::HashMap::from([(\"url\".to_string(), w3cos_core::Value::string({specifier:?}))]));",
                register(*dst)
            )
        }
        Instruction::Yield {
            value, suspension, ..
        } => match mode {
            EmissionMode::Generator | EmissionMode::AsyncGenerator => format!(
                "self.state = __W3COS_STATE__::Suspended({}); return Self::result({}, false);",
                suspension.0,
                emit_boxed_value(plan, value.0)
            ),
            EmissionMode::Async | EmissionMode::Sync => {
                bail!("ordinary W3IR AOT function contains yield")
            }
        },
        Instruction::Await {
            dst,
            value,
            suspension,
        } => {
            if !matches!(mode, EmissionMode::Async | EmissionMode::AsyncGenerator) {
                bail!("await requires async W3IR AOT emission");
            }
            let point = function
                .suspension_points
                .iter()
                .find(|point| point.id == *suspension)
                .ok_or_else(|| anyhow!("missing W3IR await suspension point"))?;
            let awaited = if matches!(mode, EmissionMode::Async) {
                format!(
                    "return Ok(Self::awaited({}, {}, {}, {}));",
                    emit_boxed_value(plan, value.0),
                    register(*dst),
                    point.resume_block.0,
                    point.reject_block.0,
                )
            } else {
                format!(
                    "return Self::awaited({}, {}, {}, {});",
                    emit_boxed_value(plan, value.0),
                    register(*dst),
                    point.resume_block.0,
                    point.reject_block.0,
                )
            };
            if matches!(mode, EmissionMode::Async) {
                awaited.replace("Self::", "__W3COS_ASYNC_FRAME__::")
            } else {
                awaited
            }
        }
        Instruction::YieldDelegate {
            dst,
            iterator,
            suspension,
        } => {
            if matches!(mode, EmissionMode::Async | EmissionMode::Sync) {
                bail!("ordinary W3IR AOT function contains yield delegation");
            }
            let point = function
                .generator_suspension_points
                .iter()
                .find(|point| point.id == *suspension)
                .ok_or_else(|| anyhow!("missing W3IR generator delegation point"))?;
            if matches!(mode, EmissionMode::AsyncGenerator) {
                format!(
                    "let iterator = {}; self.delegate_iterator = Some(iterator.clone()); match Self::delegate_call(&iterator, \"next\", Vec::new()) {{ Ok(Some(awaited)) => return Self::delegated(awaited, {}, 0), Ok(None) => {{ self.registers[{}] = w3cos_core::Value::string(\"TypeError: delegated iterator has no next method\"); self.delegate_iterator = None; self.block = {}; continue 'drive; }}, Err(reason) => {{ self.registers[{}] = reason; self.delegate_iterator = None; self.block = {}; continue 'drive; }} }}",
                    emit_boxed_value(plan, iterator.0),
                    suspension.0,
                    register(*dst),
                    point.throw_block.0,
                    register(*dst),
                    point.throw_block.0,
                )
            } else {
                format!(
                    "let iterator = {}; let delegated = Self::delegate_step(&iterator, \"next\", Vec::new()).unwrap_or_else(|| w3cos_core::throw_value(w3cos_core::Value::string(\"TypeError: delegated iterator has no next method\"))); let value = delegated.get_property(\"value\"); if delegated.get_property(\"done\").to_bool() {{ self.registers[{}] = value; self.block = {}; continue 'drive; }} self.delegate_iterator = Some(iterator); self.state = __W3COS_STATE__::SuspendedDelegate({}); return Self::result(value, false);",
                    emit_boxed_value(plan, iterator.0),
                    register(*dst),
                    point.resume_block.0,
                    suspension.0,
                )
            }
        }
        Instruction::Jump { target } => format!("self.block = {}; continue 'drive;", target.0),
        Instruction::Branch {
            condition,
            then_block,
            else_block,
        } if known_bool(plan, condition.0) => format!(
            "self.block = if {} {{ {} }} else {{ {} }}; continue 'drive;",
            emit_bool_operand(plan, condition.0),
            then_block.0,
            else_block.0
        ),
        Instruction::Branch {
            condition,
            then_block,
            else_block,
        } if num_slot(plan, condition.0).is_some() => format!(
            "self.block = if self.num_regs[{}] != 0.0_f64 && !self.num_regs[{}].is_nan() {{ {} }} else {{ {} }}; continue 'drive;",
            num_index(plan, condition.0),
            num_index(plan, condition.0),
            then_block.0,
            else_block.0
        ),
        Instruction::Branch {
            condition,
            then_block,
            else_block,
        } => format!(
            "self.block = if {}.to_bool() {{ {} }} else {{ {} }}; continue 'drive;",
            emit_boxed_value(plan, condition.0),
            then_block.0,
            else_block.0
        ),
        Instruction::Return { value } => match mode {
            EmissionMode::Generator | EmissionMode::AsyncGenerator => format!(
                "self.state = __W3COS_STATE__::Completed; return Self::result({}, true);",
                emit_boxed_use(plan, value.0, last_uses.contains(&value.0))
            ),
            EmissionMode::Sync => format!(
                "return Ok({});",
                emit_boxed_use(plan, value.0, last_uses.contains(&value.0))
            ),
            EmissionMode::Async => format!(
                "return Ok(__W3COS_ASYNC_FRAME__::completed({}));",
                emit_boxed_use(plan, value.0, last_uses.contains(&value.0))
            ),
        },
        Instruction::Throw { value } => {
            if let Some((exception, target)) = exception_target {
                format!(
                    "{}; self.block = {}; continue 'drive;",
                    store_boxed(plan, exception.0, &emit_boxed_value(plan, value.0)),
                    target.0
                )
            } else {
                match mode {
                    EmissionMode::Generator | EmissionMode::AsyncGenerator => format!(
                        "self.state = __W3COS_STATE__::Completed; return w3cos_core::throw_value({});",
                        emit_boxed_value(plan, value.0)
                    ),
                    EmissionMode::Sync | EmissionMode::Async => {
                        format!("return Err({});", emit_boxed_value(plan, value.0))
                    }
                }
            }
        }
    })
}

fn emit_constant(value: &Constant) -> String {
    match value {
        Constant::Undefined => "w3cos_core::Value::Undefined".into(),
        Constant::Null => "w3cos_core::Value::Null".into(),
        Constant::Bool(value) => format!("w3cos_core::Value::Bool({value})"),
        Constant::Number(value) => format!("w3cos_core::Value::Number({value:?})"),
        Constant::String(value) => format!("w3cos_core::Value::string({value:?})"),
    }
}

fn emit_binary_operator(
    plan: &EscapePlan,
    reaching_nums: &HashMap<u32, f64>,
    dst: u32,
    operator: BinaryOperator,
    lhs: u32,
    rhs: u32,
) -> String {
    let both = known_number(plan, reaching_nums, lhs) && known_number(plan, reaching_nums, rhs);
    let one_const = reaching_nums.contains_key(&lhs) || reaching_nums.contains_key(&rhs);
    let left = emit_f64_operand(plan, reaching_nums, lhs);
    let right = emit_f64_operand(plan, reaching_nums, rhs);
    match operator {
        BinaryOperator::Subtract | BinaryOperator::Multiply | BinaryOperator::Divide
            if both || one_const =>
        {
            let op = match operator {
                BinaryOperator::Subtract => "-",
                BinaryOperator::Multiply => "*",
                BinaryOperator::Divide => "/",
                _ => unreachable!(),
            };
            write_f64(plan, dst, &format!("{left} {op} {right}"))
        }
        BinaryOperator::Remainder if both => write_f64(plan, dst, &format!("{left} % {right}")),
        BinaryOperator::Exponentiate if both => {
            write_f64(plan, dst, &format!("{left}.powf({right})"))
        }
        BinaryOperator::LeftShift
        | BinaryOperator::SignedRightShift
        | BinaryOperator::UnsignedRightShift
        | BinaryOperator::BitwiseOr
        | BinaryOperator::BitwiseXor
        | BinaryOperator::BitwiseAnd
            if both =>
        {
            let expr = match operator {
                BinaryOperator::LeftShift => {
                    format!("(({left} as i32) << (({right} as i32) as u32 & 31)) as f64")
                }
                BinaryOperator::SignedRightShift => {
                    format!("(({left} as i32) >> (({right} as i32) as u32 & 31)) as f64")
                }
                BinaryOperator::UnsignedRightShift => {
                    format!("(({left} as u32) >> (({right} as i32) as u32 & 31)) as f64")
                }
                BinaryOperator::BitwiseOr => {
                    format!("(({left} as i32) | ({right} as i32)) as f64")
                }
                BinaryOperator::BitwiseXor => {
                    format!("(({left} as i32) ^ ({right} as i32)) as f64")
                }
                BinaryOperator::BitwiseAnd => {
                    format!("(({left} as i32) & ({right} as i32)) as f64")
                }
                _ => unreachable!(),
            };
            write_f64(plan, dst, &expr)
        }
        BinaryOperator::LessThan if both => write_bool(plan, dst, &format!("{left} < {right}")),
        BinaryOperator::LessThanOrEqual if both => {
            write_bool(plan, dst, &format!("{left} <= {right}"))
        }
        BinaryOperator::GreaterThan if both => write_bool(plan, dst, &format!("{left} > {right}")),
        BinaryOperator::GreaterThanOrEqual if both => {
            write_bool(plan, dst, &format!("{left} >= {right}"))
        }
        BinaryOperator::AbstractEqual | BinaryOperator::StrictEqual if both => {
            write_bool(plan, dst, &format!("{left} == {right}"))
        }
        operator => {
            let boxed_lhs = emit_boxed_ref(plan, lhs);
            let boxed_rhs = emit_boxed_ref(plan, rhs);
            let call = format!(
                "w3cos_core::intrinsics::{}(&{boxed_lhs}, &{boxed_rhs})",
                binary_intrinsic(operator)
            );
            if let Some(slot) = num_slot(plan, dst) {
                format!("self.num_regs[{slot}] = {call}.to_number();")
            } else if let Some(slot) = bool_slot(plan, dst) {
                format!("self.bool_regs[{slot}] = {call}.to_bool();")
            } else {
                format!("self.registers[{}] = {call};", val_index(plan, dst))
            }
        }
    }
}

#[allow(dead_code)]
fn binary_call(
    plan: &EscapePlan,
    dst: w3cos_ir::Register,
    lhs: w3cos_ir::Register,
    rhs: w3cos_ir::Register,
    intrinsic: &str,
) -> String {
    format!(
        "self.registers[{}] = w3cos_core::intrinsics::{intrinsic}(&{}, &{});",
        val_index(plan, dst.0),
        emit_boxed_value(plan, lhs.0),
        emit_boxed_value(plan, rhs.0)
    )
}

fn binary_intrinsic(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Subtract => "subtract",
        BinaryOperator::Multiply => "multiply",
        BinaryOperator::Divide => "divide",
        BinaryOperator::Remainder => "remainder",
        BinaryOperator::Exponentiate => "exponentiate",
        BinaryOperator::AbstractEqual => "abstract_equal",
        BinaryOperator::StrictEqual => "strict_equal",
        BinaryOperator::LessThan => "less_than",
        BinaryOperator::LessThanOrEqual => "less_than_or_equal",
        BinaryOperator::GreaterThan => "greater_than",
        BinaryOperator::GreaterThanOrEqual => "greater_than_or_equal",
        BinaryOperator::LeftShift => "left_shift",
        BinaryOperator::SignedRightShift => "signed_right_shift",
        BinaryOperator::UnsignedRightShift => "unsigned_right_shift",
        BinaryOperator::BitwiseOr => "bitwise_or",
        BinaryOperator::BitwiseXor => "bitwise_xor",
        BinaryOperator::BitwiseAnd => "bitwise_and",
        BinaryOperator::AbstractNotEqual
        | BinaryOperator::StrictNotEqual
        | BinaryOperator::InstanceOf
        | BinaryOperator::In => unreachable!("handled before intrinsic lookup"),
    }
}

fn unary_intrinsic(operator: UnaryOperator) -> &'static str {
    match operator {
        UnaryOperator::TypeOf => "type_of",
        UnaryOperator::Negate => "negate",
        UnaryOperator::BitwiseNot => "bitwise_not",
    }
}

fn emit_arguments_from(uses: &mut ValueUseEmitter<'_>, arguments: &[w3cos_ir::Register]) -> String {
    arguments
        .iter()
        .map(|argument| uses.emit(argument.0))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sanitize_identifier(name: &str) -> String {
    let mut output = String::new();
    for (index, character) in name.chars().enumerate() {
        if character == '_'
            || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
        {
            output.push(character);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "w3ir_generator".into()
    } else {
        output
    }
}

fn upper_camel(name: &str) -> String {
    let mut output = String::new();
    let mut uppercase = true;
    for character in name.chars() {
        if character == '_' {
            uppercase = true;
        } else if uppercase {
            output.push(character.to_ascii_uppercase());
            uppercase = false;
        } else {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::process::Command;
    use w3cos_core::Value;
    use w3cos_vm::{Limits, Vm};

    #[test]
    fn emits_a_native_state_machine_from_generator_suspension_metadata() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function* values() {
                    try {
                        const received = yield 1;
                        yield received + 2;
                    } finally {
                        yield "cleanup";
                    }
                }
                values;
            "#,
            "app:///generator-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("values"))
            .unwrap();
        let generated = generate_generator(function, "values_aot").unwrap();

        assert!(generated.contains("struct ValuesAotFrame"));
        assert!(generated.contains("ValuesAotFrameState::Suspended(0)"));
        assert!(generated.contains("w3cos_core::intrinsics::add"));
        assert!(generated.contains("pub fn values_aot("));
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));
    }

    #[test]
    fn generated_native_state_machine_matches_w3vm_without_runtime_vm_linkage() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function* values() {
                    let base = 1;
                    function* inner() {
                        yield base;
                        base = 5;
                        return base + 2;
                    }
                    const iterator = inner();
                    yield iterator.next().value;
                    const completed = iterator.next();
                    return completed.value + base;
                }
                values;
            "#,
            "app:///generator-aot-differential.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("values"))
            .unwrap()
            .clone();
        let generated = generate_generator_from_module(&module, &function, "values_aot").unwrap();
        let generator = Vm::new(module, Limits::default())
            .unwrap()
            .callable(function.id, HashMap::new())
            .unwrap()
            .call(Value::Undefined, Vec::new());
        let first = generator.call_method("next", Vec::new());
        let second = generator.call_method("next", vec![Value::Number(5.0)]);
        let cleanup = generator.call_method("return", vec![Value::Number(9.0)]);
        let completed = generator.call_method("next", Vec::new());
        let expected = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            first.get_property("value").to_js_string(),
            first.get_property("done").to_bool(),
            second.get_property("value").to_js_string(),
            second.get_property("done").to_bool(),
            cleanup.get_property("value").to_js_string(),
            cleanup.get_property("done").to_bool(),
            completed.get_property("value").to_js_string(),
            completed.get_property("done").to_bool(),
        );

        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("src")).unwrap();
        let core_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../w3cos-core")
            .canonicalize()
            .unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"w3ir_aot_generator_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {:?} }}\n",
                core_path
            ),
        )
        .unwrap();
        std::fs::write(
            fixture.path().join("src/main.rs"),
            format!(
                r#"
{generated}
fn main() {{
    let generator = values_aot(
        w3cos_core::Value::Undefined,
        &[],
        &w3cos_core::AotCaptureMap::new(),
    );
    let first = generator.call_method("next", Vec::new());
    let second = generator.call_method(
        "next",
        vec![w3cos_core::Value::Number(5.0)],
    );
    let cleanup = generator.call_method(
        "return",
        vec![w3cos_core::Value::Number(9.0)],
    );
    let completed = generator.call_method("next", Vec::new());
    println!(
        "{{}}:{{}}:{{}}:{{}}:{{}}:{{}}:{{}}:{{}}",
        first.get_property("value").to_js_string(),
        first.get_property("done").to_bool(),
        second.get_property("value").to_js_string(),
        second.get_property("done").to_bool(),
        cleanup.get_property("value").to_js_string(),
        cleanup.get_property("done").to_bool(),
        completed.get_property("value").to_js_string(),
        completed.get_property("done").to_bool(),
    );
}}
"#
            ),
        )
        .unwrap();
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "run",
                "--quiet",
                "--offline",
                "--manifest-path",
                fixture.path().join("Cargo.toml").to_str().unwrap(),
            ])
            .env(
                "CARGO_TARGET_DIR",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/w3ir-aot-fixtures"),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated AOT fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    #[test]
    fn generated_ordinary_async_state_machine_matches_w3vm_and_rejects() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                async function calculate(first, second) {
                    const left = await first;
                    async function finish(value) {
                        const right = await value;
                        return left + right;
                    }
                    return await finish(second);
                }
                calculate;
            "#,
            "app:///ordinary-async-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("calculate"))
            .unwrap()
            .clone();
        let generated =
            generate_async_function_from_module(&module, &function, "calculate_aot").unwrap();
        assert!(generated.contains("Outcome::Await"));
        assert!(generated.contains("Outcome::Complete"));
        assert!(generated.contains("w3cos_core::promise::PromiseStatus::Fulfilled"));
        assert!(!generated.contains("__w3cos_async_function_await"));
        assert!(!generated.contains("__w3cos_async_function_complete"));
        assert!(generated.contains("w3cos_core::intrinsics::await_value"));
        assert!(generated.contains("w3cos_core::intrinsics::promise_new"));
        assert!(generated.contains("w3cos_core::intrinsics::call_method"));
        assert!(!generated.contains("w3cos_core::promise::resolve"));
        assert!(!generated.contains(".call_method(\"then\""));
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));

        let callable = Vm::new(module, Limits::default())
            .unwrap()
            .callable(function.id, HashMap::new())
            .unwrap();
        let fulfilled = callable.call(
            Value::Undefined,
            vec![
                w3cos_core::promise::resolve(vec![Value::Number(2.0)]),
                w3cos_core::promise::resolve(vec![Value::Number(3.0)]),
            ],
        );
        let rejected = callable.call(
            Value::Undefined,
            vec![
                w3cos_core::promise::resolve(vec![Value::Number(2.0)]),
                w3cos_core::promise::reject(vec![Value::string("second failed")]),
            ],
        );
        w3cos_core::promise::drain_microtasks();
        let describe = |promise: &Value| match w3cos_core::promise::status(promise) {
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(value)) => {
                format!("fulfilled:{}", value.to_js_string())
            }
            Some(w3cos_core::promise::PromiseStatus::Rejected(value)) => {
                format!("rejected:{}", value.to_js_string())
            }
            _ => "pending".into(),
        };
        let expected = format!("{}|{}", describe(&fulfilled), describe(&rejected));

        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("src")).unwrap();
        let core_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../w3cos-core")
            .canonicalize()
            .unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"w3ir_aot_async_function_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {:?} }}\n",
                core_path
            ),
        )
        .unwrap();
        std::fs::write(
            fixture.path().join("src/main.rs"),
            format!(
                r#"
{generated}
fn describe(promise: &w3cos_core::Value) -> String {{
    match w3cos_core::promise::status(promise) {{
        Some(w3cos_core::promise::PromiseStatus::Fulfilled(value)) =>
            format!("fulfilled:{{}}", value.to_js_string()),
        Some(w3cos_core::promise::PromiseStatus::Rejected(value)) =>
            format!("rejected:{{}}", value.to_js_string()),
        _ => "pending".to_string(),
    }}
}}
fn main() {{
    let fulfilled = calculate_aot(
        w3cos_core::Value::Undefined,
        &[
            w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(2.0)]),
            w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(3.0)]),
        ],
        &w3cos_core::AotCaptureMap::new(),
    );
    let rejected = calculate_aot(
        w3cos_core::Value::Undefined,
        &[
            w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(2.0)]),
            w3cos_core::promise::reject(vec![w3cos_core::Value::string("second failed")]),
        ],
        &w3cos_core::AotCaptureMap::new(),
    );
    w3cos_core::promise::drain_microtasks();
    println!("{{}}|{{}}", describe(&fulfilled), describe(&rejected));
}}
"#
            ),
        )
        .unwrap();
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "run",
                "--quiet",
                "--offline",
                "--manifest-path",
                fixture.path().join("Cargo.toml").to_str().unwrap(),
            ])
            .env(
                "CARGO_TARGET_DIR",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/w3ir-aot-fixtures"),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated ordinary async AOT fixture failed:\n{}\n--- generated ---\n{generated}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    #[test]
    fn generated_async_generator_serializes_await_and_yield_requests_like_w3vm() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                async function* values(awaited) {
                    const base = await awaited;
                    const sent = yield base + 1;
                    return sent + 1;
                }
                values;
            "#,
            "app:///async-generator-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("values"))
            .unwrap()
            .clone();
        let generated = generate_generator_from_module(&module, &function, "values_aot").unwrap();
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));

        let generator = Vm::new(module, Limits::default())
            .unwrap()
            .callable(function.id, HashMap::new())
            .unwrap()
            .call(
                Value::Undefined,
                vec![w3cos_core::promise::resolve(vec![Value::Number(2.0)])],
            );
        let first = generator.call_method("next", Vec::new());
        let second = generator.call_method("next", vec![Value::Number(8.0)]);
        w3cos_core::promise::drain_microtasks();
        let describe = |promise: &Value| match w3cos_core::promise::status(promise) {
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(result)) => format!(
                "{}:{}",
                result.get_property("value").to_js_string(),
                result.get_property("done").to_bool()
            ),
            _ => panic!("unexpected async-generator Promise status"),
        };
        let expected = format!("{}:{}", describe(&first), describe(&second));

        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("src")).unwrap();
        let core_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../w3cos-core")
            .canonicalize()
            .unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"w3ir_aot_async_generator_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {:?} }}\n",
                core_path
            ),
        )
        .unwrap();
        std::fs::write(
            fixture.path().join("src/main.rs"),
            format!(
                r#"
{generated}
fn describe(promise: &w3cos_core::Value) -> String {{
    match w3cos_core::promise::status(promise) {{
        Some(w3cos_core::promise::PromiseStatus::Fulfilled(result)) => format!(
            "{{}}:{{}}",
            result.get_property("value").to_js_string(),
            result.get_property("done").to_bool(),
        ),
        _ => panic!("unexpected status"),
    }}
}}

fn main() {{
    let generator = values_aot(
        w3cos_core::Value::Undefined,
        &[w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(2.0)])],
        &w3cos_core::AotCaptureMap::new(),
    );
    let first = generator.call_method("next", Vec::new());
    let second = generator.call_method(
        "next",
        vec![w3cos_core::Value::Number(8.0)],
    );
    w3cos_core::promise::drain_microtasks();
    println!("{{}}:{{}}", describe(&first), describe(&second));
}}
"#
            ),
        )
        .unwrap();
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "run",
                "--quiet",
                "--offline",
                "--manifest-path",
                fixture.path().join("Cargo.toml").to_str().unwrap(),
            ])
            .env(
                "CARGO_TARGET_DIR",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/w3ir-aot-fixtures"),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated async-generator fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    #[test]
    fn generated_async_generator_delegates_to_a_nested_async_generator() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                async function* outer() {
                    async function* inner() {
                        try {
                            yield 1;
                            yield 2;
                        } finally {
                            yield 7;
                        }
                    }
                    const delegated = yield* inner();
                    return delegated + 1;
                }
                outer;
            "#,
            "app:///async-generator-delegate-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("outer"))
            .unwrap()
            .clone();
        let generated = generate_generator_from_module(&module, &function, "outer_aot").unwrap();
        let generator = Vm::new(module, Limits::default())
            .unwrap()
            .callable(function.id, HashMap::new())
            .unwrap()
            .call(Value::Undefined, Vec::new());
        let first = generator.call_method("next", Vec::new());
        let cleanup = generator.call_method("return", vec![Value::Number(9.0)]);
        let completed = generator.call_method("next", vec![Value::Number(11.0)]);
        w3cos_core::promise::drain_microtasks();
        let describe = |promise: &Value| match w3cos_core::promise::status(promise) {
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(result)) => format!(
                "{}:{}",
                result.get_property("value").to_js_string(),
                result.get_property("done").to_bool()
            ),
            _ => panic!("unexpected async-delegation Promise status"),
        };
        let expected = format!(
            "{}:{}:{}",
            describe(&first),
            describe(&cleanup),
            describe(&completed)
        );

        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("src")).unwrap();
        let core_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../w3cos-core")
            .canonicalize()
            .unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"w3ir_aot_async_delegate_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {:?} }}\n",
                core_path
            ),
        )
        .unwrap();
        std::fs::write(
            fixture.path().join("src/main.rs"),
            format!(
                r#"
{generated}
fn describe(promise: &w3cos_core::Value) -> String {{
    match w3cos_core::promise::status(promise) {{
        Some(w3cos_core::promise::PromiseStatus::Fulfilled(result)) => format!(
            "{{}}:{{}}",
            result.get_property("value").to_js_string(),
            result.get_property("done").to_bool(),
        ),
        _ => panic!("unexpected status"),
    }}
}}
fn main() {{
    let generator = outer_aot(
        w3cos_core::Value::Undefined,
        &[],
        &w3cos_core::AotCaptureMap::new(),
    );
    let first = generator.call_method("next", Vec::new());
    let cleanup = generator.call_method(
        "return",
        vec![w3cos_core::Value::Number(9.0)],
    );
    let completed = generator.call_method(
        "next",
        vec![w3cos_core::Value::Number(11.0)],
    );
    w3cos_core::promise::drain_microtasks();
    println!(
        "{{}}:{{}}:{{}}",
        describe(&first),
        describe(&cleanup),
        describe(&completed),
    );
}}
"#
            ),
        )
        .unwrap();
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "run",
                "--quiet",
                "--offline",
                "--manifest-path",
                fixture.path().join("Cargo.toml").to_str().unwrap(),
            ])
            .env(
                "CARGO_TARGET_DIR",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/w3ir-aot-fixtures"),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated async-delegation fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    #[test]
    fn emits_generator_exception_and_finally_control_flow_from_w3ir() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function* values() {
                    let trace = "";
                    try {
                        yield "start";
                        throw "boom";
                    } catch (error) {
                        trace += "C" + error;
                        yield trace;
                    } finally {
                        trace += "F";
                    }
                    return trace;
                }
                values;
            "#,
            "app:///generator-aot-exceptions.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("values"))
            .unwrap();
        let generated = generate_generator(function, "values_with_finally_aot").unwrap();
        assert!(generated.contains("continue 'drive"));
        assert!(generated.contains("State::Suspended"));
        assert!(!generated.contains("w3cos_vm"));
    }

    #[test]
    fn coalesces_sync_exception_capture_once_per_function() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function read(input) {
                    try {
                        const parsed = JSON.parse(input);
                        const value = parsed.value;
                        return value + 1;
                    } catch (error) {
                        return "invalid";
                    }
                }
                read;
            "#,
            "app:///sync-aot-exception-region.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("read"))
            .unwrap();
        let protected_instructions = function
            .exception_regions
            .iter()
            .flat_map(|region| region.protected_blocks.iter())
            .filter_map(|id| function.blocks.iter().find(|block| block.id == *id))
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| !is_direct_control_flow(instruction, EmissionMode::Sync))
            .count();
        let generated = generate_sync_function_from_module(&module, function, "read_aot").unwrap();

        assert!(protected_instructions > 1);
        assert!(
            !generated.contains("catch_unwind"),
            "sync try/catch AOT must not emit catch_unwind: {generated}"
        );
        assert!(
            generated.contains("w3cos_core::catch_js")
                || generated.contains("call_completion")
                || generated.contains("construct_completion"),
            "protected calls should match a JS completion: {generated}"
        );
        assert!(generated.contains("match self.block"));
    }

    #[test]
    fn routes_tdz_and_host_calls_through_completion() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function read_tdz() {
                    return x;
                    let x = 1;
                }
                read_tdz;
            "#,
            "app:///sync-aot-tdz-completion.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("read_tdz"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "read_tdz_aot").unwrap();
        assert!(
            !generated.contains("throw_value(w3cos_core::intrinsics::reference_error"),
            "local TDZ must not panic: {generated}"
        );
        assert!(
            generated.contains("return Err(")
                || generated
                    .contains("let __w3cos_exception = w3cos_core::intrinsics::reference_error"),
            "local TDZ should propagate a Completion Err: {generated}"
        );

        let call_module = crate::w3ir_lowering::lower_script(
            r#"
                function invoke(fn) {
                    return fn();
                }
                invoke;
            "#,
            "app:///sync-aot-host-call-completion.js",
        )
        .unwrap();
        let call_function = call_module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("invoke"))
            .unwrap();
        let call_generated =
            generate_sync_function_from_module(&call_module, call_function, "invoke_aot").unwrap();
        assert!(
            call_generated.contains("call_completion"),
            "sync Call should use call_completion: {call_generated}"
        );
        assert!(
            call_generated.contains("return Err(__w3cos_exception)"),
            "uncaught host throw should become run() Err: {call_generated}"
        );
    }

    #[test]
    fn emits_js_exceptions_as_result_without_catch_unwind() {
        let throw_module = crate::w3ir_lowering::lower_script(
            r#"
                function boom() {
                    throw "boom";
                }
                boom;
            "#,
            "app:///sync-aot-throw.js",
        )
        .unwrap();
        let throw_function = throw_module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("boom"))
            .unwrap();
        let throw_generated =
            generate_sync_function_from_module(&throw_module, throw_function, "boom_aot").unwrap();
        assert!(
            !throw_generated.contains("catch_unwind"),
            "throwing AOT must not emit catch_unwind: {throw_generated}"
        );
        assert!(
            throw_generated.contains("return Err("),
            "uncaught JS throw should be a Result Err: {throw_generated}"
        );

        let module = crate::w3ir_lowering::lower_script(
            r#"
                function run() {
                    try {
                        throw "boom";
                    } catch (error) {
                        return error;
                    }
                }
                run;
            "#,
            "app:///sync-aot-try-catch-throw.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("run"))
            .unwrap()
            .clone();
        let generated = generate_sync_function_from_module(&module, &function, "run_aot").unwrap();
        assert!(
            !generated.contains("catch_unwind"),
            "try/catch throw AOT must not emit catch_unwind: {generated}"
        );

        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("src")).unwrap();
        let core_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../w3cos-core")
            .canonicalize()
            .unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"w3ir_aot_try_catch_throw_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {:?} }}\n",
                core_path
            ),
        )
        .unwrap();
        std::fs::write(
            fixture.path().join("src/main.rs"),
            format!(
                r#"
{generated}
fn main() {{
    println!(
        "{{}}",
        run_aot(
            w3cos_core::Value::Undefined,
            &[],
            &w3cos_core::AotCaptureMap::new(),
        )
        .to_js_string(),
    );
}}
"#
            ),
        )
        .unwrap();
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "run",
                "--quiet",
                "--offline",
                "--manifest-path",
                fixture.path().join("Cargo.toml").to_str().unwrap(),
            ])
            .env(
                "CARGO_TARGET_DIR",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/w3ir-aot-fixtures"),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated try/catch throw fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "boom");
    }

    #[test]
    fn coalesces_straight_line_sync_blocks_before_emission() {
        let function = Function {
            id: FunctionId(0),
            name: Some("linear".into()),
            parameters: Vec::new(),
            rest_parameter: None,
            arguments_binding: None,
            bindings: Vec::new(),
            captures: Vec::new(),
            this_binding: None,
            registers: 1,
            entry: BlockId(0),
            blocks: vec![
                w3cos_ir::Block {
                    id: BlockId(0),
                    instructions: vec![
                        Instruction::LoadConstant {
                            dst: Register(0),
                            value: Constant::Number(1.0),
                        },
                        Instruction::Jump { target: BlockId(1) },
                    ],
                    source_span: None,
                },
                w3cos_ir::Block {
                    id: BlockId(1),
                    instructions: vec![Instruction::Return { value: Register(0) }],
                    source_span: None,
                },
            ],
            exception_regions: Vec::new(),
            suspension_points: Vec::new(),
            generator_suspension_points: Vec::new(),
            is_async: false,
            is_generator: false,
            source_span: None,
        };

        let blocks = coalesce_sync_blocks(&function);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, BlockId(0));
        assert!(matches!(
            blocks[0].1.as_slice(),
            [Instruction::LoadConstant { .. }, Instruction::Return { .. }]
        ));

        let generated =
            generate_sync_function_with_factories(&function, "linear", &HashMap::new(), None)
                .unwrap();
        assert!(
            !generated.contains("match self.block"),
            "a terminal single-block sync function should not retain a dispatcher: {generated}"
        );
        assert!(
            !generated.contains("block: u32"),
            "a terminal single-block sync function should not retain block state: {generated}"
        );
    }

    #[test]
    fn retains_dispatcher_for_cyclic_sync_cfg() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function count(value) {
                    let total = 0;
                    while (value > 0) {
                        total += value;
                        value -= 1;
                    }
                    return total;
                }
                count;
            "#,
            "app:///cyclic-sync.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("count"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "count_aot").unwrap();

        assert!(generated.contains("match self.block"), "{generated}");
        assert!(generated.contains("block: u32"), "{generated}");
    }

    #[test]
    fn generated_sync_function_dispatches_runtime_exceptions_to_its_handler() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function read(input) {
                    try {
                        return input.missing.value;
                    } catch (error) {
                        return "invalid";
                    }
                }
                read;
            "#,
            "app:///sync-aot-runtime-exception.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("read"))
            .unwrap()
            .clone();
        let expected = Vm::new(module.clone(), Limits::default())
            .unwrap()
            .callable(function.id, HashMap::new())
            .unwrap()
            .call(Value::Undefined, vec![Value::Null])
            .to_js_string();
        let generated = generate_sync_function_from_module(&module, &function, "read_aot").unwrap();
        assert!(
            !generated.contains("catch_unwind"),
            "runtime exception try/catch must not emit catch_unwind: {generated}"
        );

        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("src")).unwrap();
        let core_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../w3cos-core")
            .canonicalize()
            .unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"w3ir_aot_sync_exception_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {:?} }}\n",
                core_path
            ),
        )
        .unwrap();
        std::fs::write(
            fixture.path().join("src/main.rs"),
            format!(
                r#"
{generated}
fn main() {{
    println!(
        "{{}}",
        read_aot(
            w3cos_core::Value::Undefined,
            &[w3cos_core::Value::Null],
            &w3cos_core::AotCaptureMap::new(),
        )
        .to_js_string(),
    );
}}
"#
            ),
        )
        .unwrap();
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "run",
                "--quiet",
                "--offline",
                "--manifest-path",
                fixture.path().join("Cargo.toml").to_str().unwrap(),
            ])
            .env(
                "CARGO_TARGET_DIR",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/w3ir-aot-fixtures"),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated sync exception fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    #[test]
    fn emits_sync_w3ir_host_for_an_escaping_nested_generator() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function make(seed) {
                    let local = seed;
                    function bump() {
                        local = local + 1;
                        return local;
                    }
                    function* nested(delta) {
                        yield local + delta;
                        return bump();
                    }
                    return nested(2);
                }
                make;
            "#,
            "app:///sync-generator-host.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("make"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "make_aot").unwrap();
        assert!(generated.contains("struct MakeAotFrame"));
        assert!(generated.contains("make_aot__nested_"));
        assert!(generated.matches("fn run(&mut self)").count() >= 2);
        assert!(generated.contains("Rc<std::cell::RefCell"));
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));
    }

    #[test]
    fn emits_object_and_jsx_spreads_through_the_shared_core_intrinsic() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function build(source) {
                    const merged = { first: 1, ...source, last: 3 };
                    const items = [0, ...source.items, 3];
                    merged.items = items;
                    return <section {...merged} last={4}>child</section>;
                }
                build;
            "#,
            "app:///spread-aot.jsx",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("build"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "build_aot").unwrap();

        assert_eq!(
            generated
                .matches("w3cos_core::intrinsics::copy_data_properties")
                .count(),
            2
        );
        assert!(
            generated.contains("w3cos_core::intrinsics::append_iterable"),
            "array spread must use the same Core iterable intrinsic: {generated}"
        );
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));
    }

    #[test]
    fn emits_spread_calls_and_construction_through_materialized_core_arguments() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function invoke(factory, receiver, args) {
                    return [
                        factory("first", ...args),
                        receiver.run(...args),
                        new factory(...args)
                    ];
                }
                invoke;
            "#,
            "app:///call-spread-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("invoke"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "invoke_aot").unwrap();

        assert!(generated.contains("w3cos_core::intrinsics::call_with_arguments"));
        assert!(generated.contains("w3cos_core::intrinsics::call_method_with_arguments"));
        assert!(generated.contains("w3cos_core::intrinsics::construct_with_arguments"));
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));
    }

    #[test]
    fn emits_object_and_array_literals_as_arena_handles() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function build() {
                    const object = { a: 1 };
                    const items = [1];
                    return [object, items];
                }
                build;
            "#,
            "app:///literal-handles-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("build"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "build_aot").unwrap();

        assert!(
            generated.contains("w3cos_core::Value::object"),
            "object literals must intern through Value::object: {generated}"
        );
        assert!(
            generated.contains("w3cos_core::Value::array"),
            "array literals must intern through Value::array: {generated}"
        );
        assert!(
            !generated.contains("Value::Object(") && !generated.contains("Value::Array("),
            "object/array literals must not emit Value::Object/Array(Rc::new(...)): {generated}"
        );
        assert!(
            generated.contains("w3cos_core::Value::object(std::collections::HashMap::new())"),
            "empty object literals must intern via Value::object(HashMap::new()): {generated}"
        );
        assert!(
            generated.contains("w3cos_core::Value::array(vec!["),
            "array literals must intern via Value::array(vec![...]): {generated}"
        );
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));
    }

    #[test]
    fn emits_create_closure_through_aot_function_intern() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function build() {
                    const add = (value) => value + 1;
                    return add;
                }
                build;
            "#,
            "app:///closure-handles-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("build"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "build_aot").unwrap();

        assert!(
            generated.contains("w3cos_core::Value::aot_function("),
            "CreateClosure must intern through Value::aot_function: {generated}"
        );
        assert!(
            !generated.contains("Value::function(move |__this, __args|"),
            "CreateClosure must not box a dyn Fn wrapper around the factory: {generated}"
        );
        assert!(
            !generated.contains("__w3cos_nested_captures.clone()"),
            "CreateClosure must not clone captures on every call: {generated}"
        );
        assert!(
            generated.contains("__captures: &w3cos_core::AotCaptureMap"),
            "nested factory must borrow captures by ref: {generated}"
        );
        assert!(
            !generated.contains("Value::Function("),
            "CreateClosure must not emit Value::Function(Rc): {generated}"
        );
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));
    }

    #[test]
    fn emits_object_methods_and_accessors_as_core_only_w3ir_closures() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function build(seed) {
                    return {
                        seed,
                        add(value) { return this.seed + value; },
                        get value() { return this.seed; },
                        set value(next) { this.seed = next; }
                    };
                }
                build;
            "#,
            "app:///object-methods-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("build"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "build_aot").unwrap();

        assert!(generated.contains("__w3cos_getter_"));
        assert!(generated.contains("__w3cos_setter_"));
        assert!(generated.contains("w3cos_core::intrinsics::add"));
        assert!(
            generated.matches("w3cos_core::Value::aot_function").count() >= 3,
            "object methods/accessors must emit aot_function CreateClosure: {generated}"
        );
        assert!(!generated.contains("w3cos_vm"));
        assert!(!generated.contains("w3cos_ir"));
    }
    #[test]
    fn peepholes_intra_block_string_property_keys() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function read(object) {
                    object.flag = true;
                    return object.name.toUpperCase();
                }
                read;
            "#,
            "app:///const-prop-key-aot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("read"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "read_aot").unwrap();
        assert!(
            generated.contains(".get_property(\"name\")"),
            "constant GetProperty key should skip Value + to_js_string: {generated}"
        );
        assert!(
            generated.contains(".set_property(\"flag\""),
            "constant SetProperty key should skip Value + to_js_string: {generated}"
        );
        assert!(
            generated.contains(".call_method_slice(\"toUpperCase\""),
            "constant CallMethod key should skip Value + to_js_string: {generated}"
        );
        assert!(
            !generated.contains("w3cos_core::intrinsics::get_property(&self.registers"),
            "proven string keys must not go through the Value-key intrinsic: {generated}"
        );
    }

    #[test]
    fn shares_a_numeric_cell_for_a_captured_counter() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function outer() {
                    var hits = 0;
                    function tick() { hits = hits + 1; }
                    tick();
                    return hits;
                }
                outer;
            "#,
            "app:///shared-number-cell.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("outer"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "outer_aot").unwrap();
        assert!(
            generated.contains("AotCapture::Number"),
            "captured numeric var should be passed as a number cell: {generated}"
        );
        assert!(
            generated.contains("cell.set(Some(next))"),
            "increment listener should update the shared cell without a frame: {generated}"
        );
        assert!(
            generated.contains("num_cells.insert("),
            "outer frame should allocate the shared cell once: {generated}"
        );
    }

    #[test]
    fn uses_dense_binding_vectors_only_when_ids_are_zero_based() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function add(left, right) {
                    return left + right;
                }
                function outer(seed) {
                    return function inner(extra) {
                        return seed + extra;
                    };
                }
                [add, outer];
            "#,
            "app:///dense-bindings-aot.js",
        )
        .unwrap();
        let add = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("add"))
            .unwrap();
        let inner = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("inner"))
            .unwrap();
        let add_generated = generate_sync_function_from_module(&module, add, "add_aot").unwrap();
        let inner_generated =
            generate_sync_function_from_module(&module, inner, "inner_aot").unwrap();

        match slot_storage(add) {
            SlotStorage::Dense(_) => {
                assert!(
                    add_generated.contains("vec![None;")
                        || add_generated.contains("std::array::from_fn"),
                    "dense 0..n BindingIds should index a compact slot array: {add_generated}"
                );
            }
            SlotStorage::Map => {
                assert!(
                    add_generated.contains("std::collections::HashMap::new()"),
                    "non-dense add should keep HashMap slots: {add_generated}"
                );
            }
            SlotStorage::Packed(_, _) => {
                assert!(
                    add_generated.contains("std::array::from_fn"),
                    "small sparse add should pack slots: {add_generated}"
                );
            }
        }
        match slot_storage(inner) {
            SlotStorage::Dense(_) => {
                assert!(
                    inner_generated.contains("vec![None;")
                        || inner_generated.contains("std::array::from_fn"),
                    "unexpected dense inner captures: {inner_generated}"
                );
            }
            SlotStorage::Map => {
                assert!(
                    inner_generated.contains("std::collections::HashMap<"),
                    "large sparse captured BindingIds must keep HashMap slots: {inner_generated}"
                );
                assert!(
                    !inner_generated.contains("vec![None;"),
                    "sparse inner must not switch captures to Vec: {inner_generated}"
                );
            }
            SlotStorage::Packed(_, _) => {
                assert!(
                    inner_generated.contains("std::array::from_fn")
                        && inner_generated.contains("AotCapture::Cell"),
                    "a small captured binding should share the parent cell: {inner_generated}"
                );
                assert!(
                    !inner_generated.contains("std::collections::HashMap<"),
                    "small sparse inner must not allocate a HashMap frame: {inner_generated}"
                );
            }
        }
    }

    #[test]
    fn nested_closure_forwards_mutable_cell_for_outer_let() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function outer() {
                    let log = "";
                    function middle(event) {
                        const moved = event.port;
                        moved.onmessage = function inner(message) {
                            log += message.data;
                        };
                    }
                    return middle;
                }
                outer;
            "#,
            "app:///nested-let-capture.js",
        )
        .unwrap();
        let outer = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("outer"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, outer, "outer_aot").unwrap();
        assert!(
            generated.contains("AotCapture::Cell(cell)"),
            "outer let must be shared as a cell: {generated}"
        );
        assert!(
            !generated.contains("let __w3cos_pair = (getter, w3cos_core::Value::Undefined)"),
            "re-capturing a let must not freeze the cell: {generated}"
        );
    }

    #[test]
    fn delete_inlines_proven_string_key() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function removeUser(dataset) {
                    delete dataset.userId;
                    return dataset;
                }
                removeUser;
            "#,
            "app:///delete-proven-key.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("removeUser"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "remove_user_aot").unwrap();
        assert!(
            generated.contains("delete_property(") && generated.contains("Value::from(\"userId\")"),
            "delete must keep the property name after the string load is folded: {generated}"
        );
    }

    #[test]
    fn unboxes_pure_numeric_arithmetic_to_f64() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function calc() {
                    return (1 + 2) * 3 - 4 / 2;
                }
                calc;
            "#,
            "app:///numeric-escape.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("calc"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "calc_aot").unwrap();

        assert!(
            generated.contains("num_regs") && generated.contains("_f64"),
            "pure numeric function should emit f64 locals: {generated}"
        );
        assert!(
            generated.contains(" + ")
                && generated.contains(" * ")
                && generated.contains(" - ")
                && generated.contains(" / "),
            "expected f64 arithmetic operators: {generated}"
        );
        let boxed = generated.matches("w3cos_core::Value::Number").count();
        assert!(
            boxed <= 2,
            "inner arithmetic should stay unboxed; boxed {boxed} times: {generated}"
        );
        assert!(
            generated.contains("w3cos_core::Value::Number(self.num_regs")
                || generated.contains("Value::Number(self.num_regs"),
            "result must box back to Value at return: {generated}"
        );
        let ir_count = function.registers;
        let val_len = parse_undefined_vec_len(&generated);
        let num_len = parse_vec_len(&generated, "num_regs: vec![0.0_f64; ")
            .or_else(|| parse_vec_len(&generated, "num_regs: [0.0_f64; "));
        let bool_len = parse_vec_len(&generated, "bool_regs: vec![false; ")
            .or_else(|| parse_vec_len(&generated, "bool_regs: [false; "))
            .unwrap_or(0);
        assert!(
            generated.contains("num_regs: vec![0.0_f64; ")
                || generated.contains("num_regs: [0.0_f64; "),
            "compact num_regs init should be emitted: {generated}"
        );
        if let (Some(val_len), Some(num_len)) = (val_len, num_len) {
            assert!(
                val_len < ir_count,
                "Value bank must not keep a full IR-sized frame when numbers unbox; val={val_len} ir={ir_count}: {generated}"
            );
            assert_eq!(
                val_len + num_len + bool_len,
                ir_count,
                "compact banks should partition IR registers; val={val_len} num={num_len} bool={bool_len} ir={ir_count}: {generated}"
            );
        }
        assert!(!generated.contains("w3cos_vm"));
    }

    #[test]
    fn lowers_numeric_for_loop_to_native_f64_loop() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function compute() {
                    var n = 4;
                    var a = 1.1;
                    var b = 2.2;
                    var s = 0;
                    for (var i = 1; i <= n; i++) {
                        a = a + b * 0.000001;
                        b = b + a * 0.000001;
                        s = s + (a - b);
                    }
                    return s;
                }
                compute;
            "#,
            "app:///numeric-loop.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("compute"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "compute_aot").unwrap();
        assert!(
            generated.contains("loop {"),
            "simple numeric for-loop should be a native Rust loop: {generated}"
        );
        let hot = generated
            .split_once("fn __w3cos_numeric_loop")
            .and_then(|(_, rest)| {
                rest.split_once("let (")
                    .or_else(|| rest.split_once("let __w3cos_out"))
                    .map(|(hot, _)| hot)
            })
            .unwrap_or("");
        assert!(
            hot.contains("as i32") && hot.contains("while __w3cos_i <= __w3cos_limit"),
            "provable integer trip count should be an i32 counted loop: {hot}"
        );
        assert!(
            !hot.contains("Value::Number"),
            "the numeric loop must not box Value on the way out: {hot}"
        );
        assert!(
            generated.contains("num_bindings"),
            "loop-carried numbers should live in f64 bindings: {generated}"
        );
        assert!(
            !generated.contains("cell.borrow()"),
            "the numeric loop must not reload RefCell bindings: {generated}"
        );
        assert!(
            generated.contains("let mut nb_") || generated.contains("let mut nr_"),
            "the native loop should promote f64 slots to locals: {generated}"
        );
        assert!(
            generated.contains(".mul_add(")
                && generated.contains("is_x86_feature_detected!(\"fma\")")
                && generated.contains(" * "),
            "1e-6 recurrence uses fma only behind a scalar fallback: {hot}"
        );
    }

    #[test]
    fn native_loop_does_not_cache_replaced_capture_cells() {
        let module = crate::w3ir_lowering::lower_script(
            "function make() { var refs = []; for (let i = 0; i < 3; i++) { refs.push(function() { return i; }); } return refs; } make;",
            "app:///fresh-capture-loop.js",
        ).unwrap();
        let function = module
            .functions
            .iter()
            .find(|f| f.name.as_deref() == Some("make"))
            .unwrap();
        let refreshed = function
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter_map(|i| {
                if let Instruction::RefreshBinding { binding } = i {
                    Some(binding.0)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert!(
            !refreshed.is_empty(),
            "must cover a real per-iteration environment"
        );
        let generated =
            generate_sync_function_from_module(&module, function, "fresh_capture_aot").unwrap();
        for id in refreshed {
            assert!(
                !generated.contains(&format!("let __w3cos_cb_{id} =")),
                "a refreshed cell cannot be cached outside the loop: {generated}"
            );
        }
        assert!(generated.contains("let refreshed = binding.borrow().clone()"));
    }

    fn assert_fused_after_gate(generated: &str, intrinsic: &str) {
        let loop_at = generated
            .find("loop {")
            .unwrap_or_else(|| panic!("native loop missing: {generated}"));
        let break_at = generated[loop_at..]
            .find("break;")
            .unwrap_or_else(|| panic!("loop gate missing: {generated}"));
        let proof_at = generated
            .find("dom_fusion_surface_ok")
            .unwrap_or_else(|| panic!("surface proof missing: {generated}"));
        assert!(
            loop_at + break_at < proof_at,
            "the surface proof must follow the loop gate so a zero-trip body is not observed: {generated}"
        );
        assert!(
            generated.contains(intrinsic)
                && generated.contains("dom_shape_epoch")
                && generated.contains("is_undefined()")
                && generated.contains("4_u32"),
            "the benchmark shape must fuse behind the stamp and the static div id: {generated}"
        );
        assert!(
            !generated.contains("dom_build_appended"),
            "unguarded fusion must stay out of the loop: {generated}"
        );
        let mut depth = 0i32;
        for ch in generated.chars() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    assert!(depth >= 0, "unbalanced fused loop: {generated}");
                }
                _ => {}
            }
        }
        assert_eq!(depth, 0, "unbalanced fused loop: {generated}");
    }

    #[test]
    fn native_dom_update_loop_is_rewritten() {
        let module = crate::w3ir_lowering::lower_module(
            r#"
                export {};
                var DOM_NODES = 1000;
                var DOM_UPDATES = 8000;
                function domOnce() {
                    var root = document.createElement("div");
                    document.body.appendChild(root);
                    var nodes = [];
                    for (var i = 0; i < DOM_NODES; i++) {
                        var el = document.createElement("div");
                        el.id = "row-" + i;
                        el.className = "row";
                        el.textContent = "item " + i;
                        root.appendChild(el);
                        nodes.push(el);
                    }
                    var textSum = 0;
                    for (var k = 0; k < DOM_UPDATES; k++) {
                        var index = (k * 17) % DOM_NODES;
                        nodes[index].textContent = "upd " + k;
                        root.appendChild(nodes[index]);
                        textSum = textSum + nodes[index].textContent.length;
                    }
                    return textSum;
                }
            "#,
            "app:///dom-update.mjs",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("domOnce"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "dom_update_aot").unwrap();
        assert_fused_after_gate(&generated, "dom_build_guarded");
        let rewrite_at = generated
            .find("dom_rewrite_indexed_text_guarded")
            .unwrap_or_else(|| panic!("indexed rewrite missing: {generated}"));
        let loop_at = generated[..rewrite_at].rfind("loop {").unwrap_or(0);
        let gate_at = generated[loop_at..rewrite_at]
            .find("break;")
            .unwrap_or_else(|| panic!("rewrite must follow the loop gate: {generated}"));
        assert!(loop_at + gate_at < rewrite_at);
        assert!(
            generated.contains("__w3cos_rewrite_tried_")
                && generated.contains("is_undefined()")
                && generated.contains("\"upd \"")
                && generated.contains("17.0, 1000.0,")
                && generated.contains(", 8000.0,"),
            "the whole update loop is one guarded call with folded limits: {generated}"
        );
        let fallback = &generated[rewrite_at..];
        assert!(
            fallback.contains("set_property_strict(")
                && fallback.contains("\"textContent\"")
                && fallback.contains(".call_method_slice(\"appendChild\""),
            "a refused rewrite keeps the ordinary property path: {generated}"
        );
    }

    #[test]
    fn native_dom_update_with_extra_write_stays_unfused() {
        let module = crate::w3ir_lowering::lower_module(
            r#"
                export {};
                var DOM_NODES = 1000;
                var DOM_UPDATES = 8000;
                function domOnce(root, nodes) {
                    var textSum = 0;
                    for (var k = 0; k < DOM_UPDATES; k++) {
                        var index = (k * 17) % DOM_NODES;
                        nodes[index].textContent = "upd " + k;
                        nodes[index].id = "x";
                        root.appendChild(nodes[index]);
                        textSum = textSum + nodes[index].textContent.length;
                    }
                    return textSum;
                }
            "#,
            "app:///dom-update-extra.mjs",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("domOnce"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "dom_update_extra_aot").unwrap();
        assert!(
            !generated.contains("dom_rewrite_indexed_text_guarded"),
            "an extra property write stays on the ordinary path: {generated}"
        );
    }

    #[test]
    fn native_sequential_text_fill_loop_is_rewritten() {
        let module = crate::w3ir_lowering::lower_module(
            r#"
                export {};
                var TABLE_PASSES = 4;
                function tableOnce(cells, table) {
                    var checksum = 0;
                    var textSum = 0;
                    for (var pass = 0; pass < TABLE_PASSES; pass++) {
                        for (var i = 0; i < cells.length; i++) {
                            var text = "v " + pass + "-" + i;
                            cells[i].textContent = text;
                            textSum = textSum + text.length;
                        }
                        checksum = checksum + table.offsetHeight;
                    }
                    return checksum + textSum;
                }
            "#,
            "app:///table-repaint.mjs",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("tableOnce"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "table_repaint_aot").unwrap();
        let fill_at = generated
            .find("dom_fill_sequential_text_guarded")
            .unwrap_or_else(|| panic!("sequential fill missing: {generated}"));
        let loop_at = generated[..fill_at].rfind("loop {").unwrap_or(0);
        let gate_at = generated[loop_at..fill_at]
            .find("break;")
            .unwrap_or_else(|| panic!("fill must follow the loop gate: {generated}"));
        assert!(loop_at + gate_at < fill_at);
        assert!(
            generated.contains("__w3cos_fill_tried_")
                && generated.contains("is_undefined()")
                && generated.contains("\"v \"")
                && generated.contains("\"-\""),
            "the cell rewrite is one guarded call: {generated}"
        );
        assert!(
            generated.contains("offsetHeight"),
            "the height read stays outside the fused write: {generated}"
        );
    }

    #[test]
    fn native_sequential_text_fill_with_extra_write_stays_unfused() {
        let module = crate::w3ir_lowering::lower_module(
            r#"
                export {};
                function tableOnce(cells, pass) {
                    var textSum = 0;
                    for (var i = 0; i < cells.length; i++) {
                        var text = "v " + pass + "-" + i;
                        cells[i].textContent = text;
                        cells[i].id = text;
                        textSum = textSum + text.length;
                    }
                    return textSum;
                }
            "#,
            "app:///table-extra.mjs",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("tableOnce"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "table_extra_aot").unwrap();
        assert!(
            !generated.contains("dom_fill_sequential_text_guarded"),
            "an extra property write stays on the ordinary path: {generated}"
        );
    }

    #[test]
    fn native_text_append_loop_is_guarded() {
        let module = crate::w3ir_lowering::lower_module(
            r#"
                export {};
                var CSS_LIST_NODES = 1000;
                function runCssList() {
                    var root = document.createElement("div");
                    root.id = "css-list";
                    document.body.appendChild(root);
                    for (var i = 0; i < CSS_LIST_NODES; i++) {
                        var el = document.createElement("div");
                        el.textContent = "row " + (i % 40);
                        root.appendChild(el);
                    }
                    return root.offsetHeight;
                }
            "#,
            "app:///css-list.mjs",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("runCssList"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "css_list_aot").unwrap();
        assert_fused_after_gate(&generated, "dom_build_text_guarded");
        assert!(
            !generated.contains("dom_build_block_guarded")
                && !generated.contains("dom_build_flex_row_guarded"),
            "text-only rows are not the styled block path: {generated}"
        );
        assert!(
            generated.contains(".call_method_slice(\"createElement\"")
                && generated.contains("set_property_strict(")
                && generated.contains("\"textContent\"")
                && generated.contains(".call_method_slice(\"appendChild\""),
            "ordinary lookup stays on the text-append fallback: {generated}"
        );
    }

    #[test]
    fn native_dom_loop_with_binding_limit_is_guarded() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                "use strict";
                var DOM_NODES = 1000;
                function domOnce(document) {
                    var root = document.createElement("div");
                    var nodes = [];
                    for (var i = 0; i < DOM_NODES; i++) {
                        var el = document.createElement("div");
                        el.id = "row-" + i;
                        el.className = "row";
                        el.textContent = "item " + i;
                        root.appendChild(el);
                        nodes.push(el);
                    }
                    return nodes;
                }
                domOnce;
            "#,
            "app:///dom-once.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("domOnce"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "dom_once_aot").unwrap();
        assert_fused_after_gate(&generated, "dom_build_guarded");
        assert!(
            generated.contains("set_property_strict")
                && generated.contains(".call_method_slice(\"createElement\"")
                && generated.contains("dom_intern_prehashed("),
            "strict create loop keeps the observable fallback and interns the class once: {generated}"
        );
    }

    #[test]
    fn native_module_dom_list_loop_is_guarded() {
        let module = crate::w3ir_lowering::lower_module(
            r#"
                export {};
                var DOM_NODES = 1000;
                function domOnce() {
                    var root = document.createElement("div");
                    var nodes = [];
                    for (var i = 0; i < DOM_NODES; i++) {
                        var el = document.createElement("div");
                        el.id = "row-" + i;
                        el.className = "row";
                        el.textContent = "item " + i;
                        root.appendChild(el);
                        nodes.push(el);
                    }
                    return nodes;
                }
            "#,
            "app:///dom-list.mjs",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("domOnce"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "dom_list_module_aot").unwrap();
        assert_fused_after_gate(&generated, "dom_build_guarded");
    }

    #[test]
    fn native_dom_loop_keeps_observable_lookup() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function fill(document, root, nodes) {
                    for (var i = 0; i < 4; i++) {
                        var el = document.createElement("div");
                        el.id = "row-" + i;
                        el.className = "row";
                        el.textContent = "item " + i;
                        root.appendChild(el);
                        nodes.push(el);
                    }
                    return nodes;
                }
                fill;
            "#,
            "app:///dom-loop.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("fill"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "fill_aot").unwrap();
        assert_fused_after_gate(&generated, "dom_build_guarded");
        assert!(
            generated.contains("loop {"),
            "element create loop should be native: {generated}"
        );
        let loop_at = generated.find("loop {").unwrap();
        assert!(
            generated[loop_at..].contains("__w3cos_doc_"),
            "fused receivers are loaded inside the loop, after the gate: {generated}"
        );
        assert!(
            generated.contains(".call_method_slice(\"createElement\""),
            "createElement must stay an ordinary method call on the fallback: {generated}"
        );
        assert!(
            generated.contains(".call_method_slice(\"appendChild\""),
            "appendChild must stay an ordinary method call: {generated}"
        );
        assert!(
            generated.contains(".call_method_slice(\"push\""),
            "push must stay an ordinary method call: {generated}"
        );
        assert!(
            generated.contains(".set_property(\"id\""),
            "id must stay an ordinary property write: {generated}"
        );
        assert!(
            generated.contains(".set_property(\"className\""),
            "className must stay an ordinary property write: {generated}"
        );
        assert!(
            generated.contains(".set_property(\"textContent\""),
            "textContent must stay an ordinary property write: {generated}"
        );
        assert!(
            generated.contains("__w3cos_cb_"),
            "binding cells should be loaded once per loop: {generated}"
        );
        assert!(
            !generated.contains("dom_build_appended")
                && !generated.contains("dom_create_element")
                && !generated.contains("dom_set_data")
                && !generated.contains("dom_append_child")
                && !generated.contains("dom_set_style"),
            "observable DOM operations must not use unguarded intrinsics: {generated}"
        );
    }

    #[test]
    fn native_fixed_block_loop_is_guarded() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                "use strict";
                function fill(document, root) {
                    for (var i = 0; i < 4000; i++) {
                        var el = document.createElement("div");
                        el.textContent = "m " + (i % 50);
                        el.style.height = "18px";
                        root.appendChild(el);
                    }
                    return root;
                }
                fill;
            "#,
            "app:///memory-block.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("fill"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "memory_block_aot").unwrap();
        assert_fused_after_gate(&generated, "dom_build_block_guarded");
        assert!(
            generated.contains(".call_method_slice(\"createElement\"")
                && generated.contains("set_property_strict(")
                && generated.contains("\"textContent\"")
                && generated.contains("\"height\"")
                && generated.contains(".call_method_slice(\"appendChild\""),
            "ordinary lookup stays on the fallback: {generated}"
        );
    }

    #[test]
    fn native_fixed_block_module_loop_is_guarded() {
        let module = crate::w3ir_lowering::lower_module(
            r#"
                export {};
                function fill(document, root) {
                    for (var i = 0; i < 4000; i++) {
                        var el = document.createElement("div");
                        el.textContent = "m " + (i % 50);
                        el.style.height = "18px";
                        root.appendChild(el);
                    }
                    return root;
                }
            "#,
            "app:///memory-block.mjs",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("fill"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "memory_block_module_aot")
                .unwrap();
        assert_fused_after_gate(&generated, "dom_build_block_guarded");
        assert!(
            generated.contains(".call_method_slice(\"createElement\"")
                && generated.contains("set_property_strict(")
                && generated.contains("\"textContent\"")
                && generated.contains("\"height\"")
                && generated.contains(".call_method_slice(\"appendChild\""),
            "ordinary lookup stays on the module fallback: {generated}"
        );
    }

    #[test]
    fn native_column_flex_row_loop_is_guarded() {
        let source = r#"
            export {};
            var FLEX_LIST_NODES = 400;
            function runFlexList() {
                var root = document.createElement("div");
                root.style.display = "flex";
                root.style.flexDirection = "column";
                document.body.appendChild(root);
                for (var i = 0; i < FLEX_LIST_NODES; i++) {
                    var el = document.createElement("div");
                    el.textContent = "f " + (i % 20);
                    el.style.height = "18px";
                    el.style.flex = "0 0 auto";
                    root.appendChild(el);
                }
                return root.offsetHeight;
            }
        "#;
        let module = crate::w3ir_lowering::lower_module(source, "app:///flex-list.mjs").unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("runFlexList"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "flex_list_aot").unwrap();
        assert_fused_after_gate(&generated, "dom_build_flex_row_guarded");
        assert!(
            generated.contains("\"18px\"")
                && generated.contains("\"0 0 auto\"")
                && generated.contains(".call_method_slice(\"createElement\"")
                && generated.contains("set_property_strict(")
                && generated.contains("\"textContent\"")
                && generated.contains("\"height\"")
                && generated.contains("\"flex\"")
                && generated.contains(".call_method_slice(\"appendChild\""),
            "ordinary lookup stays on the flex-list fallback: {generated}"
        );
        assert!(
            !generated.contains("dom_build_block_guarded"),
            "a second style is not the single-property block path: {generated}"
        );
    }

    #[test]
    fn native_two_style_block_stays_unfused() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                "use strict";
                function fill(document, root) {
                    for (var i = 0; i < 400; i++) {
                        var el = document.createElement("div");
                        el.textContent = "m " + (i % 50);
                        el.style.height = "18px";
                        el.style.width = "40px";
                        root.appendChild(el);
                    }
                    return root;
                }
                fill;
            "#,
            "app:///two-style.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("fill"))
            .unwrap();
        let generated =
            generate_sync_function_from_module(&module, function, "two_style_aot").unwrap();
        assert!(
            !generated.contains("dom_build_flex_row_guarded")
                && !generated.contains("dom_build_block_guarded")
                && !generated.contains("dom_build_guarded")
                && !generated.contains("dom_build_text_guarded"),
            "height plus width stays on the ordinary lookup: {generated}"
        );
    }

    #[test]
    fn keeps_add_intrinsic_when_a_side_may_be_string() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function join(prefix) {
                    return prefix + 1;
                }
                join;
            "#,
            "app:///string-add-escape.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("join"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "join_aot").unwrap();
        assert!(
            generated.contains("w3cos_core::intrinsics::add"),
            "JS + must stay on the add intrinsic when an operand may be string: {generated}"
        );
    }

    #[test]
    fn unboxes_pure_bool_compare_to_bool_regs() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function pred() {
                    return 1 < 2;
                }
                pred;
            "#,
            "app:///bool-escape.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("pred"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "pred_aot").unwrap();

        assert!(
            generated.contains("bool_regs"),
            "pure bool compare should emit bool_regs: {generated}"
        );
        assert!(
            generated.contains("w3cos_core::Value::Bool(self.bool_regs")
                || generated.contains("Value::Bool(self.bool_regs"),
            "result must box back from bool_regs at return: {generated}"
        );
        // Compare result must not live only as Value::Bool stored in the Value bank.
        let boxed_bool_in_registers = generated.contains("self.registers[")
            && generated
                .lines()
                .any(|line| line.contains("self.registers[") && line.contains("Value::Bool"));
        assert!(
            !boxed_bool_in_registers,
            "compare result should not be stored as Value::Bool in registers: {generated}"
        );
        assert!(!generated.contains("w3cos_vm"));
    }

    #[test]
    fn omits_value_slot_for_unboxed_number() {
        let module = crate::w3ir_lowering::lower_script(
            r#"
                function sum() {
                    return 1 + 2 + 3;
                }
                sum;
            "#,
            "app:///omit-value-slot.js",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("sum"))
            .unwrap();
        let generated = generate_sync_function_from_module(&module, function, "sum_aot").unwrap();
        let ir_count = function.registers;
        let val_len = parse_undefined_vec_len(&generated);
        assert!(
            generated.contains("num_regs") && generated.contains("_f64"),
            "pure numeric sum should unbox to num_regs: {generated}"
        );
        assert!(
            generated.contains("w3cos_core::Value::Number(self.num_regs")
                || generated.contains("Value::Number(self.num_regs"),
            "return should box from num_regs: {generated}"
        );
        assert!(
            val_len == Some(0)
                || generated.contains("vec![w3cos_core::Value::Undefined; 0]")
                || !generated.contains("self.registers["),
            "unboxed number arithmetic must not keep Value slots; val_len={val_len:?} ir={ir_count}: {generated}"
        );
        if let Some(val_len) = val_len {
            assert!(
                val_len < ir_count,
                "Value bank must be smaller than the IR register count: {generated}"
            );
        }
    }

    fn last_use_fixture(instructions: Vec<Instruction>, registers: u32) -> Function {
        Function {
            id: FunctionId(0),
            name: Some("slice".into()),
            parameters: Vec::new(),
            rest_parameter: None,
            arguments_binding: None,
            bindings: Vec::new(),
            captures: Vec::new(),
            this_binding: None,
            registers,
            entry: BlockId(0),
            blocks: vec![w3cos_ir::Block {
                id: BlockId(0),
                instructions,
                source_span: None,
            }],
            exception_regions: Vec::new(),
            suspension_points: Vec::new(),
            generator_suspension_points: Vec::new(),
            is_async: false,
            is_generator: false,
            source_span: None,
        }
    }

    fn emit_slice(function: &Function) -> String {
        generate_sync_function_with_factories(function, "slice_aot", &HashMap::new(), None).unwrap()
    }

    #[test]
    fn moves_last_use_value_register_instead_of_cloning() {
        let function = last_use_fixture(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::String("hello".into()),
                },
                Instruction::Move {
                    dst: Register(1),
                    src: Register(0),
                },
                Instruction::Return { value: Register(1) },
            ],
            2,
        );
        let generated = emit_slice(&function);
        assert!(
            generated.contains(
                "std::mem::replace(&mut self.registers[0], w3cos_core::Value::Undefined)"
            ),
            "last-use Move of a Value slot must take instead of clone: {generated}"
        );
        assert!(
            !generated.contains("self.registers[0].clone()"),
            "last-use Move source must not clone: {generated}"
        );
        assert!(
            generated.contains("self.registers[1] = std::mem::replace"),
            "Move destination should be assigned from the taken source: {generated}"
        );
    }

    #[test]
    fn takes_last_use_call_argument_instead_of_cloning() {
        let function = last_use_fixture(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::String("callee".into()),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::Undefined,
                },
                Instruction::LoadConstant {
                    dst: Register(2),
                    value: Constant::String("arg".into()),
                },
                Instruction::Call {
                    dst: Register(3),
                    callee: Register(0),
                    this_value: Register(1),
                    arguments: vec![Register(2)],
                },
                Instruction::Return { value: Register(3) },
            ],
            4,
        );
        let generated = emit_slice(&function);
        assert!(
            generated.contains(
                "vec![std::mem::replace(&mut self.registers[2], w3cos_core::Value::Undefined)]"
            ) || generated.contains(
                "vec![std::mem::replace(&mut self.registers[2], w3cos_core::Value::Undefined),"
            ),
            "last-use Call argument must be taken, not cloned: {generated}"
        );
        assert!(
            !generated.contains("self.registers[2].clone()"),
            "last-use Call argument must not clone: {generated}"
        );
        assert!(
            generated.contains(
                "std::mem::replace(&mut self.registers[0], w3cos_core::Value::Undefined)"
            ),
            "last-use Call callee should also take: {generated}"
        );
    }

    #[test]
    fn takes_last_use_get_property_object_instead_of_cloning() {
        let function = last_use_fixture(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::String("object".into()),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::String("name".into()),
                },
                Instruction::GetProperty {
                    dst: Register(2),
                    object: Register(0),
                    key: Register(1),
                },
                Instruction::Return { value: Register(2) },
            ],
            3,
        );
        let generated = emit_slice(&function);
        assert!(
            generated.contains(".get_property(\"name\")"),
            "constant GetProperty key should still peephole: {generated}"
        );
        assert!(
            generated.contains(
                "std::mem::replace(&mut self.registers[0], w3cos_core::Value::Undefined).get_property(\"name\")"
            ),
            "last-use GetProperty object must take instead of clone: {generated}"
        );
        assert!(
            !generated.contains(".clone().get_property("),
            "last-use GetProperty object must not clone: {generated}"
        );
    }

    fn set_property_fixture(value: u32, returned: u32) -> Function {
        last_use_fixture(
            vec![
                Instruction::CreateObject {
                    dst: Register(0),
                    properties: Vec::new(),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::String("k".into()),
                },
                Instruction::CreateObject {
                    dst: Register(2),
                    properties: Vec::new(),
                },
                Instruction::SetProperty {
                    object: Register(0),
                    key: Register(1),
                    value: Register(value),
                    strict: false,
                },
                Instruction::Return {
                    value: Register(returned),
                },
            ],
            3,
        )
    }

    #[test]
    fn add_borrows_both_operands_in_place() {
        let function = last_use_fixture(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::String("a".into()),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::String("b".into()),
                },
                Instruction::Add {
                    dst: Register(2),
                    lhs: Register(0),
                    rhs: Register(1),
                },
                Instruction::Return { value: Register(2) },
            ],
            3,
        );
        let generated = emit_slice(&function);
        assert!(
            generated
                .contains("w3cos_core::intrinsics::add(&self.registers[0], &self.registers[1])"),
            "add takes &Value: no clone + drop per operand: {generated}"
        );
        assert!(
            generated.contains("return Ok(std::mem::replace(&mut self.registers[2]"),
            "the returned register is moved out: {generated}"
        );
    }

    #[test]
    fn set_property_moves_a_value_on_its_last_use() {
        let generated = emit_slice(&set_property_fixture(2, 0));
        assert!(
            generated.contains("let __w3cos_owned_2 = std::mem::replace(&mut self.registers[2], w3cos_core::Value::Undefined);"),
            "the stored value dies here, so it is moved: {generated}"
        );
        assert!(
            !generated.contains("self.registers[2].clone()"),
            "no clone of a dying value: {generated}"
        );
    }

    #[test]
    fn set_property_clones_a_value_that_is_live_afterwards() {
        let generated = emit_slice(&set_property_fixture(2, 2));
        assert!(
            !generated.contains("__w3cos_owned_2"),
            "the value is returned afterwards, so it must be cloned: {generated}"
        );
        assert!(
            generated.contains("self.registers[2].clone()"),
            "{generated}"
        );
    }

    #[test]
    fn set_property_clones_when_receiver_and_value_are_one_register() {
        // `o.self = o` at o's last use: moving the value out first would
        // leave the receiver undefined.
        let generated = emit_slice(&set_property_fixture(0, 2));
        assert!(
            !generated.contains("__w3cos_owned_0"),
            "a register read twice by one instruction is never moved: {generated}"
        );
        assert!(
            generated.contains("self.registers[0].clone()"),
            "{generated}"
        );
    }

    #[test]
    fn clones_value_register_that_is_live_after_use() {
        let function = last_use_fixture(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::String("object".into()),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::String("name".into()),
                },
                Instruction::GetProperty {
                    dst: Register(2),
                    object: Register(0),
                    key: Register(1),
                },
                Instruction::Move {
                    dst: Register(3),
                    src: Register(0),
                },
                Instruction::Return { value: Register(3) },
            ],
            4,
        );
        let generated = emit_slice(&function);
        assert!(
            generated.contains("self.registers[0].get_property(\"name\")")
                || generated.contains("get_property(&self.registers[0]"),
            "GetProperty object that is live after the use is borrowed in place: {generated}"
        );
        assert!(
            generated.contains("self.registers[3] = std::mem::replace(&mut self.registers[0], w3cos_core::Value::Undefined)")
                || generated.contains("self.registers[3] = self.registers[0].clone()"),
            "later last use of the object may take or clone: {generated}"
        );
        assert!(
            !generated.contains(
                "std::mem::replace(&mut self.registers[0], w3cos_core::Value::Undefined).get_property"
            ),
            "must not take the object at GetProperty while it is still live: {generated}"
        );
    }

    #[test]
    fn clones_call_callee_that_is_live_after_the_call() {
        let function = last_use_fixture(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::String("callee".into()),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::Undefined,
                },
                Instruction::Call {
                    dst: Register(2),
                    callee: Register(0),
                    this_value: Register(1),
                    arguments: Vec::new(),
                },
                Instruction::Return { value: Register(0) },
            ],
            3,
        );
        let generated = emit_slice(&function);
        assert!(
            generated.contains("call_completion(&self.registers[0].clone()")
                || generated.contains("call(&self.registers[0].clone()")
                || generated.contains("call_completion(&self.registers[0]")
                || generated.contains("call(&self.registers[0]"),
            "expected a Call of the live callee: {generated}"
        );
        assert!(
            generated.contains("self.registers[0].clone()"),
            "callee used after Call must be cloned at the Call (or kept): {generated}"
        );
        assert!(
            !generated.contains(
                "call(&std::mem::replace(&mut self.registers[0], w3cos_core::Value::Undefined)"
            )
                && !generated.contains(
                    "call_completion(&std::mem::replace(&mut self.registers[0], w3cos_core::Value::Undefined)"
                ),
            "must not take a callee that is returned later: {generated}"
        );
    }

    fn parse_undefined_vec_len(generated: &str) -> Option<u32> {
        parse_vec_len(generated, "registers: vec![w3cos_core::Value::Undefined; ")
            .or_else(|| parse_vec_len(generated, "registers: vec![Value::Undefined; "))
            .or_else(|| parse_vec_len(generated, "registers: [w3cos_core::Value; "))
    }

    fn parse_vec_len(generated: &str, prefix: &str) -> Option<u32> {
        let start = generated.find(prefix)?;
        let rest = &generated[start + prefix.len()..];
        let digits: String = rest
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        digits.parse().ok()
    }
}
