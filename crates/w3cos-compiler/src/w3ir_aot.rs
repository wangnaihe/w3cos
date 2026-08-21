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
/// definition is Number. Nested closures and bindings stay `Unknown`
/// (interprocedural analysis is out of scope).
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
}

fn analyze_function(function: &Function) -> EscapePlan {
    let len = function.registers as usize;
    let mut classes = vec![RegClass::Bottom; len];
    let mut changed = true;
    while changed {
        changed = false;
        let before = classes.clone();
        for block in &function.blocks {
            for instruction in &block.instructions {
                apply_register_def(&mut classes, instruction);
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
    EscapePlan {
        number,
        bools,
        num_of,
        bool_of,
        val_of,
        n_num,
        n_bool,
        n_val,
    }
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
    plan: &EscapePlan,
) -> String {
    match exception_target {
        None => dst_store.replace("__w3cos_value", expr),
        Some((exception, target)) => format!(
            "match w3cos_core::catch_js(|| {expr}) {{ Ok(__w3cos_value) => {{ {dst_store} }} Err(__w3cos_exception) => {{ {}; self.block = {}; continue 'drive; }} }}",
            store_boxed(plan, exception.0, "__w3cos_exception"),
            target.0,
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

fn apply_register_def(classes: &mut [RegClass], instruction: &Instruction) {
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
        fields.push_str("    num_regs: Vec<f64>,\n");
        inits.push_str(&format!(
            "        num_regs: vec![0.0_f64; {}],\n",
            plan.n_num
        ));
    }
    if plan.n_bool > 0 {
        fields.push_str("    bool_regs: Vec<bool>,\n");
        inits.push_str(&format!(
            "        bool_regs: vec![false; {}],\n",
            plan.n_bool
        ));
    }
    (fields, inits)
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

#[derive(Clone, Copy)]
enum SlotStorage {
    Dense(usize),
    Map,
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
        _ => SlotStorage::Map,
    }
}

fn emit_slot_fields(storage: SlotStorage) -> String {
    match storage {
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

fn emit_bindings_new(storage: SlotStorage) -> String {
    match storage {
        SlotStorage::Dense(len) => format!("    let mut bindings = vec![None; {len}];\n"),
        SlotStorage::Map => "    let mut bindings = std::collections::HashMap::new();\n".into(),
    }
}

fn emit_capture_init(storage: SlotStorage) -> String {
    match storage {
        SlotStorage::Dense(_) => (
            "    let mut capture_getters = vec![None; bindings.len()];\n    let mut capture_setters = vec![None; bindings.len()];\n    for (binding, (getter, setter)) in __captures {\n        if let Some(slot) = capture_getters.get_mut(binding as usize) {\n            *slot = Some(getter);\n        }\n        if let Some(slot) = capture_setters.get_mut(binding as usize) {\n            *slot = Some(setter);\n        }\n    }\n"
        )
        .into(),
        SlotStorage::Map => (
            "    let mut capture_getters = std::collections::HashMap::new();\n    let mut capture_setters = std::collections::HashMap::new();\n    for (binding, (getter, setter)) in __captures {\n        capture_getters.insert(binding, getter);\n        capture_setters.insert(binding, setter);\n    }\n"
        )
        .into(),
    }
}

fn emit_binding_assign(storage: SlotStorage, id: u32, value: &str) -> String {
    match storage {
        SlotStorage::Dense(_) => format!("        bindings[{id}] = Some({value});\n"),
        SlotStorage::Map => format!("        bindings.insert({id}, {value});\n"),
    }
}

fn emit_capture_get(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) => {
            format!("self.capture_getters.get({id} as usize).and_then(|slot| slot.as_ref())")
        }
        SlotStorage::Map => format!("self.capture_getters.get(&{id})"),
    }
}

fn emit_capture_get_cloned(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) => {
            format!("self.capture_getters.get({id} as usize).and_then(|slot| slot.clone())")
        }
        SlotStorage::Map => format!("self.capture_getters.get(&{id}).cloned()"),
    }
}

fn emit_capture_setter_get(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) => {
            format!("self.capture_setters.get({id} as usize).and_then(|slot| slot.as_ref())")
        }
        SlotStorage::Map => format!("self.capture_setters.get(&{id})"),
    }
}

fn emit_capture_setter_get_cloned(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) => {
            format!("self.capture_setters.get({id} as usize).and_then(|slot| slot.clone())")
        }
        SlotStorage::Map => format!("self.capture_setters.get(&{id}).cloned()"),
    }
}

fn emit_binding_get(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) => {
            format!("self.bindings.get({id} as usize).and_then(|slot| slot.as_ref())")
        }
        SlotStorage::Map => format!("self.bindings.get(&{id})"),
    }
}

fn emit_binding_get_cloned(storage: SlotStorage, id: u32) -> String {
    match storage {
        SlotStorage::Dense(_) => {
            format!("self.bindings.get({id} as usize).and_then(|slot| slot.clone())")
        }
        SlotStorage::Map => format!("self.bindings.get(&{id}).cloned()"),
    }
}

fn emit_binding_store_value(storage: SlotStorage, id: u32, rust_expr: &str) -> String {
    match storage {
        SlotStorage::Dense(_) => format!(
            "if let Some(slot) = self.bindings.get_mut({id} as usize) {{ *slot = Some(std::rc::Rc::new(std::cell::RefCell::new({rust_expr}))); }}"
        ),
        SlotStorage::Map => format!(
            "self.bindings.insert({id}, std::rc::Rc::new(std::cell::RefCell::new({rust_expr})));"
        ),
    }
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

fn read_registers(instruction: &Instruction) -> Vec<u32> {
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
        let emitted = emit_block_instructions(
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
        )?;
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
    for binding in &function.bindings {
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
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.clone()), true)))",
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
struct {type_name} {{
    registers: Vec<w3cos_core::Value>,
{num_field}{slot_fields}{block_field}
}}

impl {type_name} {{
    fn run(&mut self) -> Result<w3cos_core::Value, w3cos_core::Value> {{
{run_body}
    }}
}}

pub fn {rust_name}(
    __this: w3cos_core::Value,
    __args: Vec<w3cos_core::Value>,
    __captures: std::collections::HashMap<
        u32,
        (w3cos_core::Value, w3cos_core::Value),
    >,
) -> w3cos_core::Value {{
{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    {type_name} {{
        registers: vec![w3cos_core::Value::Undefined; {registers}],
{num_init}        bindings,
        capture_getters,
        capture_setters,
{block_initializer}
    }}.run().unwrap_or_else(|__w3cos_exception| w3cos_core::throw_value(__w3cos_exception))
}}
"#,
        registers = plan.n_val,
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
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.clone()), true)))",
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
    registers: Vec<w3cos_core::Value>,
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
    __args: Vec<w3cos_core::Value>,
    __captures: std::collections::HashMap<
        u32,
        (w3cos_core::Value, w3cos_core::Value),
    >,
) -> w3cos_core::Value {{
{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        registers: vec![w3cos_core::Value::Undefined; {registers}],
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
        registers = plan.n_val,
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
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.clone()), true)))",
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
    registers: Vec<w3cos_core::Value>,
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
    __args: Vec<w3cos_core::Value>,
    __captures: std::collections::HashMap<
        u32,
        (w3cos_core::Value, w3cos_core::Value),
    >,
) -> w3cos_core::Value {{
{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        state: {type_name}State::Start,
        registers: vec![w3cos_core::Value::Undefined; {registers}],
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
    let iterable = generator.clone();
    generator.set_property(
        "__w3cos_symbol_iterator",
        w3cos_core::Value::function(move |_, _| iterable.clone()),
    );
    generator
}}
"#,
        registers = plan.n_val,
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
            "std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.clone()), true)))",
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
    registers: Vec<w3cos_core::Value>,
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
    __args: Vec<w3cos_core::Value>,
    __captures: std::collections::HashMap<
        u32,
        (w3cos_core::Value, w3cos_core::Value),
    >,
) -> w3cos_core::Value {{
{slot_bindings_new}{binding_initializers}{parameter_initializers}{slot_capture_init}    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        state: {type_name}State::Start,
        registers: vec![w3cos_core::Value::Undefined; {registers}],
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
    let iterable = generator.clone();
    generator.set_property(
        "__w3cos_symbol_async_iterator",
        w3cos_core::Value::function(move |_, _| iterable.clone()),
    );
    generator
}}
"#,
        registers = plan.n_val,
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
    for (instruction, emitted) in instructions.iter().zip(&emitted) {
        let line = if exception_target.is_some()
            && !is_direct_control_flow(instruction, mode)
            && !is_call_like(instruction)
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

fn emit_capture_pair_store(capture_id: u32, weaken: bool) -> String {
    if !weaken {
        return format!("__w3cos_nested_captures.insert({capture_id}, __w3cos_pair);");
    }
    format!(
        "let __w3cos_target = __w3cos_pair.0.call(w3cos_core::Value::Undefined, Vec::new()); if __w3cos_target.is_object() || __w3cos_target.is_array() || __w3cos_target.is_function() {{ let __w3cos_weak = w3cos_core::intrinsics::construct(&w3cos_core::weak::weak_ref_class(), vec![__w3cos_target]); let __w3cos_getter = w3cos_core::Value::function(move |_, _| __w3cos_weak.call_method(\"deref\", Vec::new())); __w3cos_nested_captures.insert({capture_id}, (__w3cos_getter, w3cos_core::Value::Undefined)); }} else {{ __w3cos_nested_captures.insert({capture_id}, __w3cos_pair); }}"
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
        Instruction::LoadBinding { dst, binding } => format!(
            "self.registers[{}] = if let Some(getter) = {} {{ getter.call(w3cos_core::Value::Undefined, Vec::new()) }} else {{ match {} {{ Some(cell) => {{ let binding = cell.borrow(); if binding.1 {{ binding.0.clone() }} else {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")) }} }}, None => w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")) }} }};",
            register(*dst),
            emit_capture_get(storage, binding.0),
            emit_binding_get(storage, binding.0),
        ),
        Instruction::InitializeBinding { binding, value } => format!(
            "if let Some(binding) = {} {{ *binding.borrow_mut() = ({}, true); }} else {{ {} }}",
            emit_binding_get(storage, binding.0),
            emit_boxed_value(plan, value.0),
            emit_binding_store_value(
                storage,
                binding.0,
                &format!("({}, true)", emit_boxed_value(plan, value.0)),
            ),
        ),
        Instruction::StoreBinding { binding, value } => format!(
            "if let Some(setter) = {} {{ if !setter.is_callable() {{ w3cos_core::throw_value(w3cos_core::intrinsics::type_error(\"captured binding is immutable\")); }} setter.call(w3cos_core::Value::Undefined, vec![{}]); }} else if let Some(binding) = {} {{ let mut binding = binding.borrow_mut(); if !binding.1 {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")); }} binding.0 = {}; }} else {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"missing binding\")); }}",
            emit_capture_setter_get(storage, binding.0),
            emit_boxed_value(plan, value.0),
            emit_binding_get(storage, binding.0),
            emit_boxed_value(plan, value.0),
        ),
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
            emit_boxed_value(plan, lhs.0),
            emit_boxed_value(plan, rhs.0)
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
                emit_boxed_value(plan, lhs.0),
                emit_boxed_value(plan, rhs.0)
            ),
            BinaryOperator::StrictNotEqual => format!(
                "self.registers[{}] = w3cos_core::intrinsics::logical_not(&w3cos_core::intrinsics::strict_equal(&{}, &{}));",
                register(*dst),
                emit_boxed_value(plan, lhs.0),
                emit_boxed_value(plan, rhs.0)
            ),
            BinaryOperator::InstanceOf => format!(
                "self.registers[{}] = w3cos_core::intrinsics::instance_of(&{}, &{});",
                register(*dst),
                emit_boxed_value(plan, lhs.0),
                emit_boxed_value(plan, rhs.0)
            ),
            BinaryOperator::In => format!(
                "self.registers[{}] = w3cos_core::intrinsics::in_operator(&{}, &{});",
                register(*dst),
                emit_boxed_value(plan, lhs.0),
                emit_boxed_value(plan, rhs.0)
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
            emit_boxed_value(plan, value.0)
        ),
        Instruction::GetProperty { dst, object, key } => {
            if let Some(literal) = proven_strings.get(&key.0) {
                format!(
                    "self.registers[{}] = {}.get_property({literal:?});",
                    register(*dst),
                    emit_boxed_use(plan, object.0, last_uses.contains(&object.0)),
                )
            } else {
                let mut uses = ValueUseEmitter::new(plan, last_uses, [object.0, key.0]);
                format!(
                    "self.registers[{}] = w3cos_core::intrinsics::get_property(&{}, &{});",
                    register(*dst),
                    uses.emit(object.0),
                    uses.emit(key.0)
                )
            }
        }
        Instruction::DeleteProperty { dst, object, key } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::delete_property(&{}, &{});",
            register(*dst),
            emit_boxed_value(plan, object.0),
            emit_boxed_value(plan, key.0)
        ),
        Instruction::SetProperty { object, key, value } => {
            if let Some(literal) = proven_strings.get(&key.0) {
                format!(
                    "{}.set_property({literal:?}, {});",
                    emit_boxed_value(plan, object.0),
                    emit_boxed_value(plan, value.0)
                )
            } else {
                format!(
                    "w3cos_core::intrinsics::set_property(&{}, &{}, {});",
                    emit_boxed_value(plan, object.0),
                    emit_boxed_value(plan, key.0),
                    emit_boxed_value(plan, value.0)
                )
            }
        }
        Instruction::DefineField { object, key, value } => format!(
            "w3cos_core::intrinsics::define_field(&{}, &{}, {});",
            emit_boxed_value(plan, object.0),
            emit_boxed_value(plan, key.0),
            emit_boxed_value(plan, value.0)
        ),
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
        Instruction::CreateArray { dst, elements } => format!(
            "self.registers[{}] = w3cos_core::Value::array(vec![{}]);",
            register(*dst),
            elements
                .iter()
                .map(|element| emit_boxed_value(plan, element.0))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::AppendArrayElement { array, value } => format!(
            "w3cos_core::intrinsics::append_array_element(&{}, {});",
            emit_boxed_value(plan, array.0),
            emit_boxed_value(plan, value.0)
        ),
        Instruction::AppendIterable { array, iterable } => format!(
            "w3cos_core::intrinsics::append_iterable(&{}, &{});",
            emit_boxed_value(plan, array.0),
            emit_boxed_value(plan, iterable.0)
        ),
        Instruction::ArrayRest { dst, value, start } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::array_rest(&{}, {});",
            register(*dst),
            emit_boxed_value(plan, value.0),
            *start as usize
        ),
        Instruction::ObjectRest {
            dst,
            value,
            excluded,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::object_rest(&{}, &[{}]);",
            register(*dst),
            emit_boxed_value(plan, value.0),
            excluded
                .iter()
                .map(|key| emit_boxed_value(plan, key.0))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::CreateObject { dst, properties } => {
            let object = if properties.is_empty() {
                "w3cos_core::Value::object(std::collections::HashMap::new())".into()
            } else {
                format!(
                    "w3cos_core::Value::object(std::collections::HashMap::from([{}]))",
                    properties
                        .iter()
                        .map(|(key, value)| format!(
                            "({}.to_js_string(), {})",
                            emit_boxed_value(plan, key.0),
                            emit_boxed_value(plan, value.0)
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!("self.registers[{}] = {object};", register(*dst))
        }
        Instruction::CopyDataProperties { target, source } => format!(
            "w3cos_core::intrinsics::copy_data_properties(&{}, &{});",
            emit_boxed_value(plan, target.0),
            emit_boxed_value(plan, source.0)
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
                let mutable = function
                    .bindings
                    .iter()
                    .find(|binding| binding.id == *capture)
                    .is_some_and(|binding| binding.mutable);
                let local_setter = if mutable {
                    format!(
                        "{{ let cell = std::rc::Rc::clone(&cell); w3cos_core::Value::function(move |_, arguments| {{ let value = arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined); cell.borrow_mut().0 = value.clone(); value }}) }}"
                    )
                } else {
                    "w3cos_core::Value::Undefined".into()
                };
                let weaken = back_edges.weak_captures.contains(&(*nested, *capture));
                adapters.push_str(&format!(
                    "let __w3cos_pair = if let Some(getter) = {} {{ (getter, {}.unwrap_or(w3cos_core::Value::Undefined)) }} else if let Some(cell) = {} {{ let getter = {{ let cell = std::rc::Rc::clone(&cell); w3cos_core::Value::function(move |_, _| {{ let binding = cell.borrow(); if binding.1 {{ binding.0.clone() }} else {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")) }} }}) }}; let setter = {local_setter}; (getter, setter) }} else {{ return w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"missing nested generator capture\")); }}; {}",
                    emit_capture_get_cloned(storage, capture.0),
                    emit_capture_setter_get_cloned(storage, capture.0),
                    emit_binding_get_cloned(storage, capture.0),
                    emit_capture_pair_store(capture.0, weaken)
                ));
            }
            format!(
                "let mut __w3cos_nested_captures = std::collections::HashMap::new(); {adapters} self.registers[{}] = w3cos_core::Value::function(move |__this, __args| {factory}(__this, __args, __w3cos_nested_captures.clone()));",
                register(*dst)
            )
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
                "w3cos_core::intrinsics::call(&{}, {}, vec![{}])",
                uses.emit(callee.0),
                uses.emit(this_value.0),
                emit_arguments_from(&mut uses, arguments)
            );
            emit_caught_value(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
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
                "w3cos_core::intrinsics::call_with_arguments(&{}, {}, &{})",
                uses.emit(callee.0),
                uses.emit(this_value.0),
                uses.emit(arguments.0)
            );
            emit_caught_value(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
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
                    "{}.call_method({literal:?}, vec![{}])",
                    uses.emit(object.0),
                    emit_arguments_from(&mut uses, arguments)
                );
                emit_caught_value(
                    &expr,
                    &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                    exception_target,
                    plan,
                )
            } else {
                let mut reads = vec![object.0, key.0];
                reads.extend(arguments.iter().map(|argument| argument.0));
                let mut uses = ValueUseEmitter::new(plan, last_uses, reads);
                let expr = format!(
                    "w3cos_core::intrinsics::call_method(&{}, &{}, vec![{}])",
                    uses.emit(object.0),
                    uses.emit(key.0),
                    emit_arguments_from(&mut uses, arguments)
                );
                emit_caught_value(
                    &expr,
                    &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                    exception_target,
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
            let mut uses = ValueUseEmitter::new(plan, last_uses, [object.0, key.0, arguments.0]);
            let expr = format!(
                "w3cos_core::intrinsics::call_method_with_arguments(&{}, &{}, &{})",
                uses.emit(object.0),
                uses.emit(key.0),
                uses.emit(arguments.0)
            );
            emit_caught_value(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
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
                "w3cos_core::intrinsics::construct(&{}, vec![{}])",
                uses.emit(constructor.0),
                emit_arguments_from(&mut uses, arguments)
            );
            emit_caught_value(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
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
                "w3cos_core::intrinsics::construct_with_arguments(&{}, &{})",
                uses.emit(constructor.0),
                uses.emit(arguments.0)
            );
            emit_caught_value(
                &expr,
                &format!("self.registers[{}] = __w3cos_value;", register(*dst)),
                exception_target,
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
                emit_boxed_value(plan, value.0)
            ),
            EmissionMode::Sync => format!("return Ok({});", emit_boxed_value(plan, value.0)),
            EmissionMode::Async => format!(
                "return Ok(__W3COS_ASYNC_FRAME__::completed({}));",
                emit_boxed_value(plan, value.0)
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
            let boxed_lhs = emit_boxed_value(plan, lhs);
            let boxed_rhs = emit_boxed_value(plan, rhs);
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
        Vec::new(),
        std::collections::HashMap::new(),
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
        vec![
            w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(2.0)]),
            w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(3.0)]),
        ],
        std::collections::HashMap::new(),
    );
    let rejected = calculate_aot(
        w3cos_core::Value::Undefined,
        vec![
            w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(2.0)]),
            w3cos_core::promise::reject(vec![w3cos_core::Value::string("second failed")]),
        ],
        std::collections::HashMap::new(),
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
        vec![w3cos_core::promise::resolve(vec![w3cos_core::Value::Number(2.0)])],
        std::collections::HashMap::new(),
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
        Vec::new(),
        std::collections::HashMap::new(),
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
            generated.contains("w3cos_core::catch_js"),
            "protected calls should match a JS completion: {generated}"
        );
        assert!(generated.contains("match self.block"));
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
            Vec::new(),
            std::collections::HashMap::new(),
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
            vec![w3cos_core::Value::Null],
            std::collections::HashMap::new(),
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
        assert!(generated.matches("w3cos_core::Value::function").count() >= 3);
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
            generated.contains(".call_method(\"toUpperCase\""),
            "constant CallMethod key should skip Value + to_js_string: {generated}"
        );
        assert!(
            !generated.contains("w3cos_core::intrinsics::get_property(&self.registers"),
            "proven string keys must not go through the Value-key intrinsic: {generated}"
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
                    add_generated.contains("vec![None;"),
                    "dense 0..n BindingIds should index a Vec: {add_generated}"
                );
            }
            SlotStorage::Map => {
                assert!(
                    add_generated.contains("std::collections::HashMap::new()"),
                    "non-dense add should keep HashMap slots: {add_generated}"
                );
            }
        }
        match slot_storage(inner) {
            SlotStorage::Dense(_) => {
                assert!(
                    inner_generated.contains("vec![None;"),
                    "unexpected dense inner captures: {inner_generated}"
                );
            }
            SlotStorage::Map => {
                assert!(
                    inner_generated.contains("std::collections::HashMap<"),
                    "sparse captured BindingIds must keep HashMap slots: {inner_generated}"
                );
                assert!(
                    !inner_generated.contains("vec![None;"),
                    "sparse inner must not switch captures to Vec: {inner_generated}"
                );
            }
        }
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
        let num_len = parse_vec_len(&generated, "num_regs: vec![0.0_f64; ");
        let bool_len = parse_vec_len(&generated, "bool_regs: vec![false; ").unwrap_or(0);
        assert!(
            generated.contains("num_regs: vec![0.0_f64; "),
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
            generated.contains("self.registers[0].clone().get_property(\"name\")")
                || generated.contains("get_property(&self.registers[0].clone()"),
            "GetProperty object that is live after the use must still clone: {generated}"
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
            generated.contains("call(&self.registers[0].clone()")
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
            ),
            "must not take a callee that is returned later: {generated}"
        );
    }

    fn parse_undefined_vec_len(generated: &str) -> Option<u32> {
        parse_vec_len(generated, "registers: vec![w3cos_core::Value::Undefined; ")
            .or_else(|| parse_vec_len(generated, "registers: vec![Value::Undefined; "))
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
