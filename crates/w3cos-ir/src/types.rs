//! Typed W3IR (M1 item 11).
//!
//! Two layers:
//!
//! * [`FunctionTypes`] records *declared* hints: TypeScript parameter and
//!   return annotations (plus numeric default values) captured at lowering.
//!   Hints are speculation, never proof: a TS `number` parameter can still
//!   receive a string at run time, so a backend that trusts a hint must guard
//!   it (the AOT backend emits a typed version plus a generic version and
//!   checks the arguments at entry).
//! * [`infer`] computes a register / binding type for every value in a
//!   function. With `speculate = false` the result only uses facts that hold
//!   for every input; with `speculate = true` hinted parameters are assumed
//!   to have their hinted type, so the result is valid exactly when the entry
//!   guard passed.
//!
//! JS semantics notes for the lattice:
//! * `Number` is an IEEE-754 double (NaN, -0, ±Infinity included).
//! * `Int32` is only produced by operators whose result is ToInt32 by
//!   definition (`|`, `&`, `^`, `<<`, `>>`, `~` on numbers). `+`, `-`, `*`,
//!   `%`, unary `-` of Int32 operands produce `Number` (overflow, -0, NaN).
//!   `>>>` produces a uint32 and is typed `Number`.
//! * Bitwise / arithmetic results are only typed when both operands are
//!   numeric: BigInt operands produce BigInt, objects run `valueOf`.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::{
    BinaryOperator, BindingId, BindingKind, Constant, Function, Instruction, UnaryOperator,
};

/// A declared (speculative) type from source annotations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TypeHint {
    #[default]
    Unknown,
    Number,
    Bool,
    String,
    /// An object whose own data fields are known by name and declaration
    /// order (TS interface, object type literal, or class with declared
    /// fields). The shape is a hint for field-access specialization only.
    Object {
        name: Option<String>,
        fields: Vec<(String, TypeHint)>,
    },
}

impl TypeHint {
    pub fn is_unknown(&self) -> bool {
        matches!(self, TypeHint::Unknown)
    }

    /// Lattice value a guarded value of this hint takes.
    pub fn ir_type(&self) -> IrType {
        match self {
            TypeHint::Unknown => IrType::Unknown,
            TypeHint::Number => IrType::Number,
            TypeHint::Bool => IrType::Bool,
            TypeHint::String => IrType::String,
            TypeHint::Object { .. } => IrType::Object,
        }
    }
}

/// Declared hints for one function. Empty for untyped JavaScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FunctionTypes {
    /// One entry per `Function::parameters` element, by position. May be
    /// shorter than the parameter list (missing entries are `Unknown`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<TypeHint>,
    #[serde(default, skip_serializing_if = "TypeHint::is_unknown")]
    pub returns: TypeHint,
}

impl FunctionTypes {
    pub fn is_empty(&self) -> bool {
        self.returns.is_unknown() && self.parameters.iter().all(TypeHint::is_unknown)
    }

    pub fn parameter(&self, index: usize) -> &TypeHint {
        static UNKNOWN: TypeHint = TypeHint::Unknown;
        self.parameters.get(index).unwrap_or(&UNKNOWN)
    }
}

/// Inferred value type. `Bottom` means "no definition seen" (unreachable or
/// not yet visited during the fixpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IrType {
    Bottom,
    /// A Number proven to be an integer in [-2^31, 2^31) and never -0.
    Int32,
    Number,
    Bool,
    String,
    /// Any object (including functions, arrays, class instances).
    Object,
    Unknown,
}

impl IrType {
    pub fn join(self, other: IrType) -> IrType {
        use IrType::*;
        match (self, other) {
            (Bottom, x) | (x, Bottom) => x,
            (a, b) if a == b => a,
            (Int32, Number) | (Number, Int32) => Number,
            _ => Unknown,
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, IrType::Int32 | IrType::Number)
    }

    /// A concrete, non-`Unknown` type (a "typed node").
    pub fn is_typed(self) -> bool {
        !matches!(self, IrType::Unknown | IrType::Bottom)
    }
}

/// Result of [`infer`].
#[derive(Debug, Clone, PartialEq)]
pub struct TypedFunction {
    pub registers: Vec<IrType>,
    pub bindings: HashMap<BindingId, IrType>,
    pub returns: IrType,
    /// Parameter positions whose hint was used as an assumption (only with
    /// `speculate = true`). A backend must guard exactly these.
    pub speculated_parameters: Vec<usize>,
}

impl TypedFunction {
    /// Registers that received at least one definition.
    pub fn defined_registers(&self) -> usize {
        self.registers
            .iter()
            .filter(|t| **t != IrType::Bottom)
            .count()
    }

    pub fn typed_registers(&self) -> usize {
        self.registers.iter().filter(|t| t.is_typed()).count()
    }

    pub fn numeric_registers(&self) -> usize {
        self.registers.iter().filter(|t| t.is_numeric()).count()
    }
}

/// Bindings written by a nested closure (or by an outer function, for this
/// function's own captures) are opaque to a per-function analysis.
fn opaque_bindings(function: &Function) -> HashSet<BindingId> {
    let mut opaque: HashSet<BindingId> = function.captures.iter().copied().collect();
    for block in &function.blocks {
        for instruction in &block.instructions {
            if let Instruction::CreateClosure { captures, .. } = instruction {
                opaque.extend(captures.iter().copied());
            }
        }
    }
    opaque
}

/// Parameters a backend may speculate on: a non-`Unknown` hint, not captured
/// by a closure, and no `arguments` object (sloppy-mode mapped arguments
/// alias parameters).
pub fn speculable_parameters(function: &Function, types: &FunctionTypes) -> Vec<usize> {
    if function.arguments_binding.is_some() || function.is_generator || function.is_async {
        return Vec::new();
    }
    let opaque = opaque_bindings(function);
    function
        .parameters
        .iter()
        .enumerate()
        .filter(|(index, binding)| {
            !types.parameter(*index).is_unknown() && !opaque.contains(binding)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Flow-insensitive register / binding type inference for one function.
///
/// Binding types are the join of every store (plus a seed for bindings that
/// may be observed before any store), so a register loaded from a binding is
/// typed only when every write to that binding is.
pub fn infer(function: &Function, speculate: bool) -> TypedFunction {
    let opaque = opaque_bindings(function);
    let speculated = if speculate {
        speculable_parameters(function, &function.types)
    } else {
        Vec::new()
    };
    let mut bindings: HashMap<BindingId, IrType> = HashMap::new();
    let local: HashMap<BindingId, BindingKind> = function
        .bindings
        .iter()
        .map(|binding| (binding.id, binding.kind))
        .collect();
    let mut seed = |id: BindingId, ty: IrType| {
        let entry = bindings.entry(id).or_insert(IrType::Bottom);
        *entry = entry.join(ty);
    };
    for (index, parameter) in function.parameters.iter().enumerate() {
        let ty = if speculated.contains(&index) {
            function.types.parameter(index).ir_type()
        } else {
            IrType::Unknown
        };
        seed(*parameter, ty);
    }
    if let Some(rest) = function.rest_parameter {
        seed(rest, IrType::Object);
    }
    if let Some(arguments) = function.arguments_binding {
        seed(arguments, IrType::Object);
    }
    if let Some(this) = function.this_binding {
        seed(this, IrType::Unknown);
    }
    for binding in &function.bindings {
        match binding.kind {
            // Hoisted `var` reads `undefined` before its first store; catch
            // parameters and imports are written outside the IR.
            BindingKind::Var | BindingKind::Import | BindingKind::Catch => {
                seed(binding.id, IrType::Unknown)
            }
            BindingKind::Function => seed(binding.id, IrType::Object),
            _ => {}
        }
        if opaque.contains(&binding.id) {
            seed(binding.id, IrType::Unknown);
        }
    }
    for id in &opaque {
        seed(*id, IrType::Unknown);
    }

    let mut registers = vec![IrType::Bottom; function.registers as usize];
    // Catch / finally registers are written by the unwinder.
    for region in &function.exception_regions {
        if let Some(slot) = registers.get_mut(region.exception.0 as usize) {
            *slot = IrType::Unknown;
        }
    }
    let mut returns = IrType::Bottom;
    loop {
        let mut changed = false;
        for block in &function.blocks {
            for instruction in &block.instructions {
                changed |= transfer(
                    instruction,
                    &mut registers,
                    &mut bindings,
                    &local,
                    &mut returns,
                );
            }
        }
        if !changed {
            break;
        }
    }
    TypedFunction {
        registers,
        bindings,
        returns,
        speculated_parameters: speculated,
    }
}

fn get(registers: &[IrType], register: crate::Register) -> IrType {
    registers
        .get(register.0 as usize)
        .copied()
        .unwrap_or(IrType::Unknown)
}

fn def(registers: &mut [IrType], register: crate::Register, ty: IrType) -> bool {
    let Some(slot) = registers.get_mut(register.0 as usize) else {
        return false;
    };
    let next = slot.join(ty);
    if next != *slot {
        *slot = next;
        true
    } else {
        false
    }
}

fn transfer(
    instruction: &Instruction,
    registers: &mut [IrType],
    bindings: &mut HashMap<BindingId, IrType>,
    local: &HashMap<BindingId, BindingKind>,
    returns: &mut IrType,
) -> bool {
    use IrType::*;
    match instruction {
        Instruction::LoadConstant { dst, value } => {
            let ty = match value {
                Constant::Number(n) => {
                    if n.fract() == 0.0
                        && *n >= i32::MIN as f64
                        && *n <= i32::MAX as f64
                        && !(*n == 0.0 && n.is_sign_negative())
                    {
                        Int32
                    } else {
                        Number
                    }
                }
                Constant::Bool(_) => Bool,
                Constant::String(_) => String,
                Constant::Undefined | Constant::Null => Unknown,
            };
            def(registers, *dst, ty)
        }
        Instruction::Move { dst, src } => {
            let ty = get(registers, *src);
            def(registers, *dst, ty)
        }
        Instruction::LoadBinding { dst, binding } => {
            // Bindings declared outside this function (module / global
            // scope) are written elsewhere.
            let ty = if local.contains_key(binding) || bindings.contains_key(binding) {
                bindings.get(binding).copied().unwrap_or(Bottom)
            } else {
                Unknown
            };
            def(registers, *dst, ty)
        }
        Instruction::InitializeBinding { binding, value }
        | Instruction::StoreBinding { binding, value } => {
            let ty = if local.contains_key(binding) {
                get(registers, *value)
            } else {
                Unknown
            };
            let entry = bindings.entry(*binding).or_insert(Bottom);
            let next = entry.join(ty);
            if next != *entry {
                *entry = next;
                true
            } else {
                false
            }
        }
        Instruction::Add { dst, lhs, rhs } => {
            let (l, r) = (get(registers, *lhs), get(registers, *rhs));
            let primitive = |t: IrType| matches!(t, Int32 | Number | Bool | String);
            let ty = if l.is_numeric() && r.is_numeric() {
                Number
            } else if (l == String && primitive(r)) || (r == String && primitive(l)) {
                String
            } else if l == Bottom || r == Bottom {
                Bottom
            } else {
                Unknown
            };
            def(registers, *dst, ty)
        }
        Instruction::Binary {
            dst,
            operator,
            lhs,
            rhs,
        } => {
            let (l, r) = (get(registers, *lhs), get(registers, *rhs));
            let numeric = l.is_numeric() && r.is_numeric();
            let pending = l == Bottom || r == Bottom;
            let ty = match operator {
                BinaryOperator::LessThan
                | BinaryOperator::LessThanOrEqual
                | BinaryOperator::GreaterThan
                | BinaryOperator::GreaterThanOrEqual
                | BinaryOperator::AbstractEqual
                | BinaryOperator::AbstractNotEqual
                | BinaryOperator::StrictEqual
                | BinaryOperator::StrictNotEqual
                | BinaryOperator::InstanceOf
                | BinaryOperator::In => Bool,
                BinaryOperator::BitwiseOr
                | BinaryOperator::BitwiseXor
                | BinaryOperator::BitwiseAnd
                | BinaryOperator::LeftShift
                | BinaryOperator::SignedRightShift
                    if numeric =>
                {
                    Int32
                }
                BinaryOperator::Subtract
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Remainder
                | BinaryOperator::Exponentiate
                | BinaryOperator::UnsignedRightShift
                    if numeric =>
                {
                    Number
                }
                _ if pending => Bottom,
                _ => Unknown,
            };
            def(registers, *dst, ty)
        }
        Instruction::Unary {
            dst,
            operator,
            value,
        } => {
            let v = get(registers, *value);
            let ty = match operator {
                UnaryOperator::TypeOf => String,
                // -(Int32) may be -0 or 2^31.
                UnaryOperator::Negate if v.is_numeric() => Number,
                UnaryOperator::BitwiseNot if v.is_numeric() => Int32,
                _ if v == Bottom => Bottom,
                _ => Unknown,
            };
            def(registers, *dst, ty)
        }
        Instruction::DeleteProperty { dst, .. } | Instruction::HasPrivate { dst, .. } => {
            def(registers, *dst, Bool)
        }
        Instruction::CreateObject { dst, .. }
        | Instruction::CreateArray { dst, .. }
        | Instruction::ArrayRest { dst, .. }
        | Instruction::ObjectRest { dst, .. }
        | Instruction::CreateClosure { dst, .. }
        | Instruction::CreateClass { dst, .. } => def(registers, *dst, Object),
        Instruction::Return { value } => {
            let next = returns.join(get(registers, *value));
            if next != *returns {
                *returns = next;
                true
            } else {
                false
            }
        }
        other => match crate::written_register(other) {
            Some(dst) => def(registers, dst, Unknown),
            None => false,
        },
    }
}

/// Module-wide typed-W3IR coverage (for reports and the AOT summary).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    pub functions: usize,
    /// Functions with at least one declared parameter / return hint.
    pub hinted_functions: usize,
    /// Functions with at least one parameter a backend may speculate on.
    pub speculable_functions: usize,
    pub registers: usize,
    /// Registers with a concrete type without any speculation.
    pub proven_typed_registers: usize,
    /// Registers with a concrete type assuming speculated parameters hold.
    pub speculative_typed_registers: usize,
    pub proven_numeric_registers: usize,
    pub speculative_numeric_registers: usize,
}

pub fn coverage(module: &crate::Module) -> Coverage {
    let mut out = Coverage::default();
    for function in &module.functions {
        out.functions += 1;
        if !function.types.is_empty() {
            out.hinted_functions += 1;
        }
        let proven = infer(function, false);
        let speculative = infer(function, true);
        if !speculative.speculated_parameters.is_empty() {
            out.speculable_functions += 1;
        }
        out.registers += proven.defined_registers();
        out.proven_typed_registers += proven.typed_registers();
        out.speculative_typed_registers += speculative.typed_registers();
        out.proven_numeric_registers += proven.numeric_registers();
        out.speculative_numeric_registers += speculative.numeric_registers();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Binding, Block, BlockId, FunctionId, Register};

    fn function(instructions: Vec<Instruction>, registers: u32, types: FunctionTypes) -> Function {
        Function {
            id: FunctionId(1),
            name: Some("f".into()),
            parameters: vec![BindingId(0), BindingId(1)],
            rest_parameter: None,
            arguments_binding: None,
            bindings: vec![
                Binding {
                    id: BindingId(0),
                    name: "*parameter:0*".into(),
                    kind: BindingKind::Parameter,
                    mutable: true,
                },
                Binding {
                    id: BindingId(1),
                    name: "*parameter:1*".into(),
                    kind: BindingKind::Parameter,
                    mutable: true,
                },
            ],
            captures: Vec::new(),
            this_binding: None,
            registers,
            entry: BlockId(0),
            blocks: vec![Block {
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
            types,
        }
    }

    fn numbers() -> FunctionTypes {
        FunctionTypes {
            parameters: vec![TypeHint::Number, TypeHint::Number],
            returns: TypeHint::Number,
        }
    }

    fn load(dst: u32, binding: u32) -> Instruction {
        Instruction::LoadBinding {
            dst: Register(dst),
            binding: BindingId(binding),
        }
    }

    fn binary(dst: u32, operator: BinaryOperator, lhs: u32, rhs: u32) -> Instruction {
        Instruction::Binary {
            dst: Register(dst),
            operator,
            lhs: Register(lhs),
            rhs: Register(rhs),
        }
    }

    #[test]
    fn join_is_a_lattice() {
        use IrType::*;
        assert_eq!(Bottom.join(Int32), Int32);
        assert_eq!(Int32.join(Number), Number);
        assert_eq!(Number.join(Bool), Unknown);
        assert_eq!(String.join(String), String);
        assert_eq!(Unknown.join(Bottom), Unknown);
    }

    #[test]
    fn speculation_types_parameters_only_when_asked() {
        let f = function(
            vec![
                load(0, 0),
                load(1, 1),
                binary(2, BinaryOperator::Multiply, 0, 1),
                Instruction::Return { value: Register(2) },
            ],
            3,
            numbers(),
        );
        let proven = infer(&f, false);
        assert_eq!(proven.registers[2], IrType::Unknown);
        assert_eq!(proven.returns, IrType::Unknown);
        assert!(proven.speculated_parameters.is_empty());
        let speculative = infer(&f, true);
        assert_eq!(speculative.registers[2], IrType::Number);
        assert_eq!(speculative.returns, IrType::Number);
        assert_eq!(speculative.speculated_parameters, vec![0, 1]);
    }

    #[test]
    fn int32_only_from_bitwise_operators() {
        let f = function(
            vec![
                load(0, 0),
                load(1, 1),
                binary(2, BinaryOperator::BitwiseOr, 0, 1),
                binary(3, BinaryOperator::BitwiseAnd, 2, 2),
                // Int32 + Int32 may overflow.
                Instruction::Add {
                    dst: Register(4),
                    lhs: Register(2),
                    rhs: Register(3),
                },
                // -(Int32) may be -0 / 2^31.
                Instruction::Unary {
                    dst: Register(5),
                    operator: UnaryOperator::Negate,
                    value: Register(2),
                },
                binary(6, BinaryOperator::UnsignedRightShift, 2, 3),
                binary(7, BinaryOperator::Remainder, 2, 3),
                Instruction::Return { value: Register(4) },
            ],
            8,
            numbers(),
        );
        let t = infer(&f, true);
        assert_eq!(t.registers[2], IrType::Int32);
        assert_eq!(t.registers[3], IrType::Int32);
        assert_eq!(t.registers[4], IrType::Number);
        assert_eq!(t.registers[5], IrType::Number);
        assert_eq!(t.registers[6], IrType::Number);
        assert_eq!(t.registers[7], IrType::Number);
    }

    #[test]
    fn constants_negative_zero_and_fractions_are_not_int32() {
        let f = function(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::Number(-0.0),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::Number(1.5),
                },
                Instruction::LoadConstant {
                    dst: Register(2),
                    value: Constant::Number(7.0),
                },
                Instruction::LoadConstant {
                    dst: Register(3),
                    value: Constant::Number(4294967296.0),
                },
                Instruction::LoadConstant {
                    dst: Register(4),
                    value: Constant::Number(f64::NAN),
                },
                Instruction::Return { value: Register(2) },
            ],
            5,
            FunctionTypes::default(),
        );
        let t = infer(&f, false);
        assert_eq!(
            &t.registers[..],
            &[
                IrType::Number,
                IrType::Number,
                IrType::Int32,
                IrType::Number,
                IrType::Number
            ]
        );
    }

    #[test]
    fn string_concatenation_and_unknown_add() {
        let f = function(
            vec![
                Instruction::LoadConstant {
                    dst: Register(0),
                    value: Constant::String("a".into()),
                },
                load(1, 0),
                Instruction::Add {
                    dst: Register(2),
                    lhs: Register(0),
                    rhs: Register(1),
                },
                load(3, 1),
                Instruction::Add {
                    dst: Register(4),
                    lhs: Register(3),
                    rhs: Register(1),
                },
                Instruction::Return { value: Register(2) },
            ],
            5,
            FunctionTypes::default(),
        );
        let t = infer(&f, true);
        // Unknown + string may call ToPrimitive on an object: not a string
        // type we can prove from the operand types alone.
        assert_eq!(t.registers[2], IrType::Unknown);
        assert_eq!(t.registers[4], IrType::Unknown);
    }

    #[test]
    fn arguments_object_or_capture_disables_speculation() {
        let mut f = function(
            vec![load(0, 0), Instruction::Return { value: Register(0) }],
            1,
            numbers(),
        );
        assert_eq!(speculable_parameters(&f, &f.types), vec![0, 1]);
        f.arguments_binding = Some(BindingId(9));
        f.bindings.push(Binding {
            id: BindingId(9),
            name: "arguments".into(),
            kind: BindingKind::Var,
            mutable: true,
        });
        assert!(speculable_parameters(&f, &f.types).is_empty());

        let mut g = function(
            vec![
                Instruction::CreateClosure {
                    dst: Register(1),
                    function: FunctionId(2),
                    captures: vec![BindingId(1)],
                },
                load(0, 0),
                Instruction::Return { value: Register(0) },
            ],
            2,
            numbers(),
        );
        assert_eq!(speculable_parameters(&g, &g.types), vec![0]);
        g.is_async = true;
        assert!(speculable_parameters(&g, &g.types).is_empty());
    }

    #[test]
    fn binding_types_join_every_store() {
        let mut f = function(
            vec![
                load(0, 0),
                Instruction::StoreBinding {
                    binding: BindingId(5),
                    value: Register(0),
                },
                Instruction::LoadConstant {
                    dst: Register(1),
                    value: Constant::String("s".into()),
                },
                Instruction::StoreBinding {
                    binding: BindingId(6),
                    value: Register(0),
                },
                Instruction::StoreBinding {
                    binding: BindingId(6),
                    value: Register(1),
                },
                load(2, 5),
                load(3, 6),
                Instruction::Return { value: Register(2) },
            ],
            4,
            numbers(),
        );
        for id in [5, 6] {
            f.bindings.push(Binding {
                id: BindingId(id),
                name: format!("b{id}"),
                kind: BindingKind::Let,
                mutable: true,
            });
        }
        let t = infer(&f, true);
        assert_eq!(t.registers[2], IrType::Number);
        assert_eq!(t.registers[3], IrType::Unknown);
    }
}
