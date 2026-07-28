//! Versioned, backend-neutral JavaScript intermediate representation.
//!
//! This crate deliberately has no dependency on SWC, the DOM, the native
//! runtime, or a VM. Both the AOT backend and dynamic executor consume this
//! contract so JavaScript semantics have one reviewable lowering layer.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

pub const FORMAT_VERSION: u32 = 17;

macro_rules! id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub u32);
    };
}

id!(Register);
id!(BlockId);
id!(FunctionId);
id!(BindingId);
id!(SuspensionId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub source: String,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constant {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOperator {
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Exponentiate,
    AbstractEqual,
    AbstractNotEqual,
    StrictEqual,
    StrictNotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    LeftShift,
    SignedRightShift,
    UnsignedRightShift,
    BitwiseOr,
    BitwiseXor,
    BitwiseAnd,
    InstanceOf,
    In,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOperator {
    TypeOf,
    Negate,
    BitwiseNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingKind {
    Var,
    Let,
    Const,
    Function,
    Class,
    Import,
    Parameter,
    Catch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub id: BindingId,
    pub name: String,
    pub kind: BindingKind,
    pub mutable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    pub specifier: String,
    /// `default`, `*`, or an exported binding name.
    pub imported: String,
    pub local: BindingId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    pub exported: String,
    pub local: BindingId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Instruction {
    LoadConstant {
        dst: Register,
        value: Constant,
    },
    Move {
        dst: Register,
        src: Register,
    },
    LoadBinding {
        dst: Register,
        binding: BindingId,
    },
    /// Performs the declaration-time transition out of the temporal dead
    /// zone. Ordinary assignment uses `StoreBinding` and must not initialize
    /// an uninitialized lexical binding.
    InitializeBinding {
        binding: BindingId,
        value: Register,
    },
    StoreBinding {
        binding: BindingId,
        value: Register,
    },
    /// Replaces a lexical binding's current cell with a fresh cell carrying
    /// the same value. Closures that already captured the previous cell keep
    /// it, enabling ECMAScript per-iteration `for (let ...)` environments and
    /// fresh nested block/switch environments on repeated entry.
    RefreshBinding {
        binding: BindingId,
    },
    Add {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Binary {
        dst: Register,
        operator: BinaryOperator,
        lhs: Register,
        rhs: Register,
    },
    Unary {
        dst: Register,
        operator: UnaryOperator,
        value: Register,
    },
    GetProperty {
        dst: Register,
        object: Register,
        key: Register,
    },
    DeleteProperty {
        dst: Register,
        object: Register,
        key: Register,
    },
    SetProperty {
        object: Register,
        key: Register,
        value: Register,
    },
    /// Defines an own data property without invoking an inherited setter.
    /// Class field initializers use ECMAScript `[[Define]]`, not `[[Set]]`.
    DefineField {
        object: Register,
        key: Register,
        value: Register,
    },
    DefinePrivate {
        object: Register,
        brand: Register,
        name: Register,
        value: Register,
    },
    GetPrivate {
        dst: Register,
        object: Register,
        brand: Register,
        name: Register,
    },
    SetPrivate {
        object: Register,
        brand: Register,
        name: Register,
        value: Register,
    },
    HasPrivate {
        dst: Register,
        object: Register,
        brand: Register,
    },
    DefinePrivateMethod {
        brand: Register,
        name: Register,
        value: Register,
    },
    DefinePrivateAccessor {
        brand: Register,
        name: Register,
        getter: Option<Register>,
        setter: Option<Register>,
    },
    CreateObject {
        dst: Register,
        properties: Vec<(Register, Register)>,
    },
    /// Copies the source's own enumerable string properties onto an existing
    /// object using ECMAScript CopyDataProperties semantics.
    CopyDataProperties {
        target: Register,
        source: Register,
    },
    CreateArray {
        dst: Register,
        elements: Vec<Register>,
    },
    /// Appends one already-evaluated value to an incrementally constructed
    /// array.
    AppendArrayElement {
        array: Register,
        value: Register,
    },
    /// Exhausts an iterable and appends its yielded values to an incrementally
    /// constructed array.
    AppendIterable {
        array: Register,
        iterable: Register,
    },
    ArrayRest {
        dst: Register,
        value: Register,
        start: u32,
    },
    ObjectRest {
        dst: Register,
        value: Register,
        excluded: Vec<Register>,
    },
    CreateClosure {
        dst: Register,
        function: FunctionId,
        captures: Vec<BindingId>,
    },
    CreateClass {
        dst: Register,
        constructor: Register,
        super_class: Option<Register>,
        initializer: Option<Register>,
    },
    Call {
        dst: Register,
        callee: Register,
        this_value: Register,
        arguments: Vec<Register>,
    },
    /// Calls a value with arguments materialized incrementally in an array.
    CallWithArguments {
        dst: Register,
        callee: Register,
        this_value: Register,
        arguments: Register,
    },
    /// Calls a property while preserving the receiver and routing built-in
    /// methods through the shared semantic core. This is distinct from
    /// loading a method value and later calling it without its base object.
    CallMethod {
        dst: Register,
        object: Register,
        key: Register,
        arguments: Vec<Register>,
    },
    /// Receiver-preserving method call with incrementally materialized
    /// arguments.
    CallMethodWithArguments {
        dst: Register,
        object: Register,
        key: Register,
        arguments: Register,
    },
    Construct {
        dst: Register,
        constructor: Register,
        arguments: Vec<Register>,
    },
    ConstructWithArguments {
        dst: Register,
        constructor: Register,
        arguments: Register,
    },
    DynamicImport {
        dst: Register,
        specifier: Register,
    },
    ImportMeta {
        dst: Register,
    },
    Await {
        dst: Register,
        value: Register,
        suspension: SuspensionId,
    },
    /// Suspends a generator and exposes `value` as the next iterator result.
    /// On `.next(input)`, `dst` receives `input` and execution resumes at the
    /// point's `resume_block`; `.throw` and `.return` select their dedicated
    /// completion-injection blocks.
    Yield {
        dst: Register,
        value: Register,
        suspension: SuspensionId,
    },
    /// Delegates iterator protocol operations for `yield*`. The generator VM
    /// forwards `.next`, `.throw`, and `.return` while the delegate is active,
    /// then writes the delegate's terminal value to `dst`.
    YieldDelegate {
        dst: Register,
        iterator: Register,
        suspension: SuspensionId,
    },
    Jump {
        target: BlockId,
    },
    Branch {
        condition: Register,
        then_block: BlockId,
        else_block: BlockId,
    },
    Return {
        value: Register,
    },
    Throw {
        value: Register,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub instructions: Vec<Instruction>,
    pub source_span: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExceptionRegion {
    pub protected_blocks: Vec<BlockId>,
    pub catch_block: Option<BlockId>,
    pub finally_block: Option<BlockId>,
    pub exception: Register,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspensionPoint {
    pub id: SuspensionId,
    pub await_block: BlockId,
    pub resume_block: BlockId,
    pub reject_block: BlockId,
    pub live_registers: Vec<Register>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratorSuspensionPoint {
    pub id: SuspensionId,
    pub yield_block: BlockId,
    pub result: Register,
    pub resume_block: BlockId,
    pub throw_block: BlockId,
    pub return_block: BlockId,
    pub live_registers: Vec<Register>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Function {
    pub id: FunctionId,
    pub name: Option<String>,
    pub parameters: Vec<BindingId>,
    /// Optional final parameter initialized to a fresh array containing all
    /// arguments after `parameters`.
    #[serde(default)]
    pub rest_parameter: Option<BindingId>,
    pub bindings: Vec<Binding>,
    /// Outer lexical cells captured by this function.
    pub captures: Vec<BindingId>,
    pub this_binding: Option<BindingId>,
    pub registers: u32,
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    pub exception_regions: Vec<ExceptionRegion>,
    pub suspension_points: Vec<SuspensionPoint>,
    #[serde(default)]
    pub generator_suspension_points: Vec<GeneratorSuspensionPoint>,
    pub is_async: bool,
    #[serde(default)]
    pub is_generator: bool,
    pub source_span: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Module {
    pub format_version: u32,
    pub specifier: String,
    /// Static module requests in source order, including side-effect-only
    /// imports and re-export dependencies.
    pub requested_modules: Vec<String>,
    /// Module requests whose non-default exports are forwarded by
    /// `export * from "..."`.
    #[serde(default)]
    pub star_exports: Vec<String>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub functions: Vec<Function>,
    pub entry: FunctionId,
}

impl Module {
    pub fn new(specifier: impl Into<String>, entry: FunctionId, functions: Vec<Function>) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            specifier: specifier.into(),
            requested_modules: Vec::new(),
            star_exports: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions,
            entry,
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.format_version != FORMAT_VERSION {
            return Err(ValidationError::UnsupportedVersion(self.format_version));
        }
        if self.specifier.is_empty() {
            return Err(ValidationError::EmptyModuleSpecifier);
        }
        if self.requested_modules.iter().any(String::is_empty) {
            return Err(ValidationError::EmptyRequestedModule);
        }
        if self.star_exports.iter().any(String::is_empty) {
            return Err(ValidationError::EmptyStarExport);
        }
        if self
            .star_exports
            .iter()
            .any(|specifier| !self.requested_modules.contains(specifier))
        {
            return Err(ValidationError::UnrequestedStarExport);
        }

        let mut function_ids = HashSet::new();
        let mut function_captures = HashMap::new();
        let mut binding_ids = HashSet::new();
        for function in &self.functions {
            if !function_ids.insert(function.id) {
                return Err(ValidationError::DuplicateFunction(function.id));
            }
            function_captures.insert(function.id, function.captures.as_slice());
            for binding in &function.bindings {
                if !binding_ids.insert(binding.id) {
                    return Err(ValidationError::DuplicateBinding(binding.id));
                }
            }
        }
        if !function_ids.contains(&self.entry) {
            return Err(ValidationError::MissingEntry(self.entry));
        }

        let mut imported_locals = HashSet::new();
        for import in &self.imports {
            if import.specifier.is_empty() {
                return Err(ValidationError::EmptyImportSpecifier(import.local));
            }
            if !binding_ids.contains(&import.local) {
                return Err(ValidationError::MissingBinding(import.local));
            }
            if !imported_locals.insert(import.local) {
                return Err(ValidationError::DuplicateImport(import.local));
            }
        }
        let mut export_names = HashSet::new();
        for export in &self.exports {
            if !binding_ids.contains(&export.local) {
                return Err(ValidationError::MissingBinding(export.local));
            }
            if !export_names.insert(&export.exported) {
                return Err(ValidationError::DuplicateExport(export.exported.clone()));
            }
        }

        for function in &self.functions {
            function.validate(&function_ids, &function_captures, &binding_ids)?;
        }
        Ok(())
    }
}

impl Function {
    fn validate(
        &self,
        function_ids: &HashSet<FunctionId>,
        function_captures: &HashMap<FunctionId, &[BindingId]>,
        module_bindings: &HashSet<BindingId>,
    ) -> Result<(), ValidationError> {
        let local_bindings: HashSet<_> = self.bindings.iter().map(|binding| binding.id).collect();
        let local_binding_kinds: HashMap<_, _> = self
            .bindings
            .iter()
            .map(|binding| (binding.id, binding.kind))
            .collect();
        let captures: HashSet<_> = self.captures.iter().copied().collect();
        if captures.len() != self.captures.len() {
            return Err(ValidationError::DuplicateCapture(self.id));
        }
        for capture in &self.captures {
            if !module_bindings.contains(capture) || local_bindings.contains(capture) {
                return Err(ValidationError::InvalidCapture(self.id, *capture));
            }
        }
        let mut parameter_bindings = HashSet::new();
        for parameter in &self.parameters {
            if !parameter_bindings.insert(*parameter) {
                return Err(ValidationError::DuplicateFunctionParameter(
                    self.id, *parameter,
                ));
            }
            if local_binding_kinds.get(parameter) != Some(&BindingKind::Parameter) {
                return Err(ValidationError::MissingFunctionBinding(self.id, *parameter));
            }
        }
        if let Some(rest_parameter) = self.rest_parameter {
            if local_binding_kinds.get(&rest_parameter) != Some(&BindingKind::Parameter) {
                return Err(ValidationError::MissingFunctionBinding(
                    self.id,
                    rest_parameter,
                ));
            }
            if parameter_bindings.contains(&rest_parameter) {
                return Err(ValidationError::DuplicateFunctionParameter(
                    self.id,
                    rest_parameter,
                ));
            }
        }
        if let Some(this_binding) = self.this_binding
            && !local_bindings.contains(&this_binding)
        {
            return Err(ValidationError::MissingFunctionBinding(
                self.id,
                this_binding,
            ));
        }
        let accessible_bindings: HashSet<_> = local_bindings.union(&captures).copied().collect();

        let mut blocks = HashSet::new();
        for block in &self.blocks {
            if !blocks.insert(block.id) {
                return Err(ValidationError::DuplicateBlock(self.id, block.id));
            }
            if let Some(span) = &block.source_span {
                validate_span(span)?;
            }
        }
        if !blocks.contains(&self.entry) {
            return Err(ValidationError::MissingEntryBlock(self.id, self.entry));
        }
        if let Some(span) = &self.source_span {
            validate_span(span)?;
        }

        let suspension_ids: HashSet<_> = self
            .suspension_points
            .iter()
            .map(|point| point.id)
            .collect();
        if suspension_ids.len() != self.suspension_points.len() {
            return Err(ValidationError::DuplicateSuspension(self.id));
        }
        if !self.is_async && !self.suspension_points.is_empty() {
            return Err(ValidationError::SuspensionInSyncFunction(self.id));
        }
        for point in &self.suspension_points {
            validate_block(self.id, point.await_block, &blocks)?;
            validate_block(self.id, point.resume_block, &blocks)?;
            validate_block(self.id, point.reject_block, &blocks)?;
            for register in &point.live_registers {
                validate_register(self.id, *register, self.registers)?;
            }
        }
        let generator_suspension_ids: HashSet<_> = self
            .generator_suspension_points
            .iter()
            .map(|point| point.id)
            .collect();
        if generator_suspension_ids.len() != self.generator_suspension_points.len() {
            return Err(ValidationError::DuplicateGeneratorSuspension(self.id));
        }
        if !self.is_generator && !self.generator_suspension_points.is_empty() {
            return Err(ValidationError::GeneratorSuspensionInOrdinaryFunction(
                self.id,
            ));
        }
        for point in &self.generator_suspension_points {
            validate_block(self.id, point.yield_block, &blocks)?;
            validate_register(self.id, point.result, self.registers)?;
            validate_block(self.id, point.resume_block, &blocks)?;
            validate_block(self.id, point.throw_block, &blocks)?;
            validate_block(self.id, point.return_block, &blocks)?;
            for register in &point.live_registers {
                validate_register(self.id, *register, self.registers)?;
            }
        }
        for region in &self.exception_regions {
            if region.catch_block.is_none() && region.finally_block.is_none() {
                return Err(ValidationError::EmptyExceptionRegion(self.id));
            }
            if region.protected_blocks.is_empty() {
                return Err(ValidationError::EmptyProtectedBlocks(self.id));
            }
            for block in &region.protected_blocks {
                validate_block(self.id, *block, &blocks)?;
            }
            if let Some(block) = region.catch_block {
                validate_block(self.id, block, &blocks)?;
            }
            if let Some(block) = region.finally_block {
                validate_block(self.id, block, &blocks)?;
            }
            validate_register(self.id, region.exception, self.registers)?;
        }

        for block in &self.blocks {
            if !block.instructions.last().is_some_and(is_terminator) {
                return Err(ValidationError::UnterminatedBlock(self.id, block.id));
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                if is_terminator(instruction) && index + 1 != block.instructions.len() {
                    return Err(ValidationError::InstructionAfterTerminator(
                        self.id, block.id,
                    ));
                }
                for target in branch_targets(instruction) {
                    validate_block(self.id, target, &blocks)?;
                }
                for register in registers(instruction) {
                    validate_register(self.id, register, self.registers)?;
                }
                for binding in bindings(instruction) {
                    if !accessible_bindings.contains(&binding) {
                        return Err(ValidationError::MissingFunctionBinding(self.id, binding));
                    }
                }
                match instruction {
                    Instruction::RefreshBinding { binding }
                        if !matches!(
                            local_binding_kinds.get(binding),
                            Some(BindingKind::Let | BindingKind::Const | BindingKind::Class)
                        ) =>
                    {
                        return Err(ValidationError::InvalidRefreshBinding(self.id, *binding));
                    }
                    Instruction::CreateClosure {
                        function, captures, ..
                    } => {
                        if !function_ids.contains(function) {
                            return Err(ValidationError::MissingFunction(*function));
                        }
                        if captures
                            .iter()
                            .any(|binding| !accessible_bindings.contains(binding))
                            || function_captures
                                .get(function)
                                .is_none_or(|expected| *expected != captures.as_slice())
                        {
                            return Err(ValidationError::InvalidClosureCapture(self.id));
                        }
                    }
                    Instruction::Await { suspension, .. } => {
                        if !self.is_async || !suspension_ids.contains(suspension) {
                            return Err(ValidationError::InvalidAwait(self.id, *suspension));
                        }
                    }
                    Instruction::Yield { suspension, .. }
                    | Instruction::YieldDelegate { suspension, .. } => {
                        if !self.is_generator || !generator_suspension_ids.contains(suspension) {
                            return Err(ValidationError::InvalidYield(self.id, *suspension));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

fn validate_span(span: &SourceSpan) -> Result<(), ValidationError> {
    if span.source.is_empty() || span.start > span.end {
        Err(ValidationError::InvalidSourceSpan)
    } else {
        Ok(())
    }
}

fn validate_block(
    function: FunctionId,
    block: BlockId,
    blocks: &HashSet<BlockId>,
) -> Result<(), ValidationError> {
    if blocks.contains(&block) {
        Ok(())
    } else {
        Err(ValidationError::MissingTarget(function, block))
    }
}

fn validate_register(
    function: FunctionId,
    register: Register,
    register_count: u32,
) -> Result<(), ValidationError> {
    if register.0 < register_count {
        Ok(())
    } else {
        Err(ValidationError::RegisterOutOfBounds(
            function,
            register,
            register_count,
        ))
    }
}

fn is_terminator(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::Jump { .. }
            | Instruction::Branch { .. }
            | Instruction::Return { .. }
            | Instruction::Throw { .. }
    )
}

fn branch_targets(instruction: &Instruction) -> Vec<BlockId> {
    match instruction {
        Instruction::Jump { target } => vec![*target],
        Instruction::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        _ => Vec::new(),
    }
}

fn bindings(instruction: &Instruction) -> Vec<BindingId> {
    match instruction {
        Instruction::LoadBinding { binding, .. }
        | Instruction::InitializeBinding { binding, .. }
        | Instruction::StoreBinding { binding, .. }
        | Instruction::RefreshBinding { binding } => vec![*binding],
        _ => Vec::new(),
    }
}

fn registers(instruction: &Instruction) -> Vec<Register> {
    match instruction {
        Instruction::LoadConstant { dst, .. } | Instruction::ImportMeta { dst } => vec![*dst],
        Instruction::Move { dst, src } => vec![*dst, *src],
        Instruction::LoadBinding { dst, .. } => vec![*dst],
        Instruction::InitializeBinding { value, .. } | Instruction::StoreBinding { value, .. } => {
            vec![*value]
        }
        Instruction::RefreshBinding { .. } => Vec::new(),
        Instruction::Add { dst, lhs, rhs } => vec![*dst, *lhs, *rhs],
        Instruction::Binary { dst, lhs, rhs, .. } => vec![*dst, *lhs, *rhs],
        Instruction::Unary { dst, value, .. } => vec![*dst, *value],
        Instruction::GetProperty { dst, object, key }
        | Instruction::DeleteProperty { dst, object, key } => vec![*dst, *object, *key],
        Instruction::SetProperty { object, key, value }
        | Instruction::DefineField { object, key, value } => vec![*object, *key, *value],
        Instruction::DefinePrivate {
            object,
            brand,
            name,
            value,
        }
        | Instruction::SetPrivate {
            object,
            brand,
            name,
            value,
        } => vec![*object, *brand, *name, *value],
        Instruction::GetPrivate {
            dst,
            object,
            brand,
            name,
        } => vec![*dst, *object, *brand, *name],
        Instruction::HasPrivate { dst, object, brand } => vec![*dst, *object, *brand],
        Instruction::DefinePrivateMethod { brand, name, value } => {
            vec![*brand, *name, *value]
        }
        Instruction::DefinePrivateAccessor {
            brand,
            name,
            getter,
            setter,
        } => {
            let mut values = vec![*brand, *name];
            values.extend(getter);
            values.extend(setter);
            values
        }
        Instruction::CreateObject { dst, properties } => {
            let mut values = vec![*dst];
            for (key, value) in properties {
                values.push(*key);
                values.push(*value);
            }
            values
        }
        Instruction::CopyDataProperties { target, source } => vec![*target, *source],
        Instruction::CreateArray { dst, elements } => {
            let mut values = vec![*dst];
            values.extend(elements);
            values
        }
        Instruction::AppendArrayElement { array, value } => vec![*array, *value],
        Instruction::AppendIterable { array, iterable } => vec![*array, *iterable],
        Instruction::ArrayRest { dst, value, .. } => vec![*dst, *value],
        Instruction::ObjectRest {
            dst,
            value,
            excluded,
        } => {
            let mut values = vec![*dst, *value];
            values.extend(excluded);
            values
        }
        Instruction::CreateClosure { dst, .. } => vec![*dst],
        Instruction::CreateClass {
            dst,
            constructor,
            super_class,
            initializer,
        } => {
            let mut values = vec![*dst, *constructor];
            values.extend(super_class);
            values.extend(initializer);
            values
        }
        Instruction::Call {
            dst,
            callee,
            this_value,
            arguments,
        } => {
            let mut values = vec![*dst, *callee, *this_value];
            values.extend(arguments);
            values
        }
        Instruction::CallWithArguments {
            dst,
            callee,
            this_value,
            arguments,
        } => vec![*dst, *callee, *this_value, *arguments],
        Instruction::CallMethod {
            dst,
            object,
            key,
            arguments,
        } => {
            let mut values = vec![*dst, *object, *key];
            values.extend(arguments);
            values
        }
        Instruction::CallMethodWithArguments {
            dst,
            object,
            key,
            arguments,
        } => vec![*dst, *object, *key, *arguments],
        Instruction::Construct {
            dst,
            constructor,
            arguments,
        } => {
            let mut values = vec![*dst, *constructor];
            values.extend(arguments);
            values
        }
        Instruction::ConstructWithArguments {
            dst,
            constructor,
            arguments,
        } => vec![*dst, *constructor, *arguments],
        Instruction::DynamicImport { dst, specifier } => vec![*dst, *specifier],
        Instruction::Await { dst, value, .. } | Instruction::Yield { dst, value, .. } => {
            vec![*dst, *value]
        }
        Instruction::YieldDelegate { dst, iterator, .. } => vec![*dst, *iterator],
        Instruction::Branch { condition, .. } => vec![*condition],
        Instruction::Return { value } | Instruction::Throw { value } => vec![*value],
        Instruction::Jump { .. } => Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    UnsupportedVersion(u32),
    EmptyModuleSpecifier,
    EmptyRequestedModule,
    EmptyStarExport,
    UnrequestedStarExport,
    EmptyImportSpecifier(BindingId),
    DuplicateFunction(FunctionId),
    MissingFunction(FunctionId),
    MissingEntry(FunctionId),
    DuplicateBinding(BindingId),
    MissingBinding(BindingId),
    DuplicateImport(BindingId),
    DuplicateExport(String),
    DuplicateCapture(FunctionId),
    InvalidCapture(FunctionId, BindingId),
    InvalidClosureCapture(FunctionId),
    MissingFunctionBinding(FunctionId, BindingId),
    DuplicateFunctionParameter(FunctionId, BindingId),
    InvalidRefreshBinding(FunctionId, BindingId),
    DuplicateBlock(FunctionId, BlockId),
    MissingEntryBlock(FunctionId, BlockId),
    UnterminatedBlock(FunctionId, BlockId),
    InstructionAfterTerminator(FunctionId, BlockId),
    MissingTarget(FunctionId, BlockId),
    RegisterOutOfBounds(FunctionId, Register, u32),
    EmptyExceptionRegion(FunctionId),
    EmptyProtectedBlocks(FunctionId),
    DuplicateSuspension(FunctionId),
    SuspensionInSyncFunction(FunctionId),
    InvalidAwait(FunctionId, SuspensionId),
    DuplicateGeneratorSuspension(FunctionId),
    GeneratorSuspensionInOrdinaryFunction(FunctionId),
    InvalidYield(FunctionId, SuspensionId),
    InvalidSourceSpan,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u32, instructions: Vec<Instruction>) -> Block {
        Block {
            id: BlockId(id),
            instructions,
            source_span: None,
        }
    }

    fn valid_module() -> Module {
        Module::new(
            "app:///main.js",
            FunctionId(0),
            vec![Function {
                id: FunctionId(0),
                name: Some("main".into()),
                parameters: Vec::new(),
                rest_parameter: None,
                bindings: Vec::new(),
                captures: Vec::new(),
                this_binding: None,
                registers: 4,
                entry: BlockId(0),
                blocks: vec![block(
                    0,
                    vec![
                        Instruction::LoadConstant {
                            dst: Register(0),
                            value: Constant::Number(42.0),
                        },
                        Instruction::Unary {
                            dst: Register(1),
                            operator: UnaryOperator::TypeOf,
                            value: Register(0),
                        },
                        Instruction::Unary {
                            dst: Register(2),
                            operator: UnaryOperator::BitwiseNot,
                            value: Register(0),
                        },
                        Instruction::Binary {
                            dst: Register(3),
                            operator: BinaryOperator::BitwiseAnd,
                            lhs: Register(0),
                            rhs: Register(2),
                        },
                        Instruction::Return { value: Register(3) },
                    ],
                )],
                exception_regions: Vec::new(),
                suspension_points: Vec::new(),
                generator_suspension_points: Vec::new(),
                is_async: false,
                is_generator: false,
                source_span: None,
            }],
        )
    }

    #[test]
    fn accepts_a_versioned_terminated_module() {
        assert_eq!(valid_module().validate(), Ok(()));
    }

    #[test]
    fn validates_and_roundtrips_generator_resume_edges() {
        let mut module = valid_module();
        let function = &mut module.functions[0];
        function.registers = 2;
        function.blocks = vec![
            block(
                0,
                vec![
                    Instruction::LoadConstant {
                        dst: Register(0),
                        value: Constant::String("yielded".into()),
                    },
                    Instruction::Yield {
                        dst: Register(1),
                        value: Register(0),
                        suspension: SuspensionId(0),
                    },
                    Instruction::Jump { target: BlockId(1) },
                ],
            ),
            block(1, vec![Instruction::Return { value: Register(1) }]),
            block(2, vec![Instruction::Throw { value: Register(1) }]),
            block(3, vec![Instruction::Return { value: Register(1) }]),
        ];
        function.generator_suspension_points = vec![GeneratorSuspensionPoint {
            id: SuspensionId(0),
            yield_block: BlockId(0),
            result: Register(1),
            resume_block: BlockId(1),
            throw_block: BlockId(2),
            return_block: BlockId(3),
            live_registers: vec![Register(0)],
        }];
        function.is_generator = true;

        assert_eq!(module.validate(), Ok(()));
        let encoded = serde_json::to_string(&module).unwrap();
        let decoded: Module = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, module);

        module.functions[0].is_generator = false;
        assert_eq!(
            module.validate(),
            Err(ValidationError::GeneratorSuspensionInOrdinaryFunction(
                FunctionId(0)
            ))
        );

        module.functions[0].is_generator = true;
        module.functions[0].is_async = true;
        assert_eq!(module.validate(), Ok(()));
    }

    #[test]
    fn rejects_empty_static_module_requests() {
        let mut module = valid_module();
        module.requested_modules.push(String::new());
        assert_eq!(
            module.validate(),
            Err(ValidationError::EmptyRequestedModule)
        );
    }

    #[test]
    fn validates_star_exports_as_static_module_requests() {
        let mut module = valid_module();
        module.star_exports.push("./dependency.js".into());
        assert_eq!(
            module.validate(),
            Err(ValidationError::UnrequestedStarExport)
        );

        module.requested_modules.push("./dependency.js".into());
        assert_eq!(module.validate(), Ok(()));
    }

    #[test]
    fn format_roundtrips_without_losing_identity() {
        let module = valid_module();
        let encoded = serde_json::to_vec(&module).expect("serialize W3IR");
        let decoded: Module = serde_json::from_slice(&encoded).expect("deserialize W3IR");
        assert_eq!(decoded, module);
        assert_eq!(decoded.validate(), Ok(()));
    }

    #[test]
    fn accepts_lexical_closure_exception_module_and_async_metadata() {
        let outer = Binding {
            id: BindingId(0),
            name: "outer".into(),
            kind: BindingKind::Let,
            mutable: true,
        };
        let imported = Binding {
            id: BindingId(1),
            name: "dependency".into(),
            kind: BindingKind::Import,
            mutable: false,
        };
        let mut module = Module::new(
            "app:///main.js",
            FunctionId(0),
            vec![
                Function {
                    id: FunctionId(0),
                    name: Some("main".into()),
                    parameters: Vec::new(),
                    rest_parameter: None,
                    bindings: vec![outer.clone(), imported],
                    captures: Vec::new(),
                    this_binding: None,
                    registers: 3,
                    entry: BlockId(0),
                    blocks: vec![
                        block(
                            0,
                            vec![
                                Instruction::CreateClosure {
                                    dst: Register(0),
                                    function: FunctionId(1),
                                    captures: vec![outer.id],
                                },
                                Instruction::Await {
                                    dst: Register(1),
                                    value: Register(0),
                                    suspension: SuspensionId(0),
                                },
                                Instruction::Jump { target: BlockId(1) },
                            ],
                        ),
                        block(1, vec![Instruction::Return { value: Register(1) }]),
                        block(2, vec![Instruction::Throw { value: Register(2) }]),
                    ],
                    exception_regions: vec![ExceptionRegion {
                        protected_blocks: vec![BlockId(0)],
                        catch_block: Some(BlockId(2)),
                        finally_block: None,
                        exception: Register(2),
                    }],
                    suspension_points: vec![SuspensionPoint {
                        id: SuspensionId(0),
                        await_block: BlockId(0),
                        resume_block: BlockId(1),
                        reject_block: BlockId(2),
                        live_registers: vec![Register(0)],
                    }],
                    generator_suspension_points: Vec::new(),
                    is_async: true,
                    is_generator: false,
                    source_span: Some(SourceSpan {
                        source: "app:///main.js".into(),
                        start: 0,
                        end: 50,
                    }),
                },
                Function {
                    id: FunctionId(1),
                    name: Some("inner".into()),
                    parameters: Vec::new(),
                    rest_parameter: None,
                    bindings: Vec::new(),
                    captures: vec![outer.id],
                    this_binding: None,
                    registers: 1,
                    entry: BlockId(0),
                    blocks: vec![block(
                        0,
                        vec![
                            Instruction::LoadBinding {
                                dst: Register(0),
                                binding: outer.id,
                            },
                            Instruction::Return { value: Register(0) },
                        ],
                    )],
                    exception_regions: Vec::new(),
                    suspension_points: Vec::new(),
                    generator_suspension_points: Vec::new(),
                    is_async: false,
                    is_generator: false,
                    source_span: None,
                },
            ],
        );
        module.imports.push(Import {
            specifier: "./dependency.js".into(),
            imported: "default".into(),
            local: BindingId(1),
        });
        module.exports.push(Export {
            exported: "result".into(),
            local: outer.id,
        });
        assert_eq!(module.validate(), Ok(()));
    }

    #[test]
    fn rejects_unknown_versions_before_execution() {
        let mut module = valid_module();
        module.format_version += 1;
        assert_eq!(
            module.validate(),
            Err(ValidationError::UnsupportedVersion(FORMAT_VERSION + 1))
        );
    }

    #[test]
    fn rejects_invalid_registers_and_branch_targets() {
        let mut module = valid_module();
        module.functions[0].blocks[0].instructions = vec![Instruction::Branch {
            condition: Register(2),
            then_block: BlockId(0),
            else_block: BlockId(9),
        }];
        assert_eq!(
            module.validate(),
            Err(ValidationError::MissingTarget(FunctionId(0), BlockId(9)))
        );
    }

    #[test]
    fn rejects_instructions_after_a_terminator() {
        let mut module = valid_module();
        module.functions[0].blocks[0]
            .instructions
            .insert(1, Instruction::Return { value: Register(0) });
        assert_eq!(
            module.validate(),
            Err(ValidationError::InstructionAfterTerminator(
                FunctionId(0),
                BlockId(0)
            ))
        );
    }

    #[test]
    fn rejects_closures_whose_capture_abi_does_not_match_the_target() {
        let binding = Binding {
            id: BindingId(0),
            name: "outer".into(),
            kind: BindingKind::Let,
            mutable: true,
        };
        let mut module = valid_module();
        module.functions[0].bindings.push(binding.clone());
        module.functions[0].blocks[0].instructions.insert(
            1,
            Instruction::CreateClosure {
                dst: Register(0),
                function: FunctionId(1),
                captures: Vec::new(),
            },
        );
        module.functions.push(Function {
            id: FunctionId(1),
            name: Some("inner".into()),
            parameters: Vec::new(),
            rest_parameter: None,
            bindings: Vec::new(),
            captures: vec![binding.id],
            this_binding: None,
            registers: 1,
            entry: BlockId(0),
            blocks: vec![block(
                0,
                vec![
                    Instruction::LoadBinding {
                        dst: Register(0),
                        binding: binding.id,
                    },
                    Instruction::Return { value: Register(0) },
                ],
            )],
            exception_regions: Vec::new(),
            suspension_points: Vec::new(),
            generator_suspension_points: Vec::new(),
            is_async: false,
            is_generator: false,
            source_span: None,
        });
        assert_eq!(
            module.validate(),
            Err(ValidationError::InvalidClosureCapture(FunctionId(0)))
        );
    }

    #[test]
    fn validates_rest_parameter_as_a_distinct_local_binding() {
        let parameter = Binding {
            id: BindingId(0),
            name: "rest".into(),
            kind: BindingKind::Parameter,
            mutable: true,
        };
        let mut module = valid_module();
        module.functions[0].bindings.push(parameter.clone());
        module.functions[0].rest_parameter = Some(parameter.id);
        assert_eq!(module.validate(), Ok(()));
        let encoded = serde_json::to_vec(&module).expect("serialize rest-parameter W3IR");
        let decoded: Module =
            serde_json::from_slice(&encoded).expect("deserialize rest-parameter W3IR");
        assert_eq!(decoded.functions[0].rest_parameter, Some(parameter.id));
        assert_eq!(decoded.validate(), Ok(()));

        module.functions[0].parameters.push(parameter.id);
        assert_eq!(
            module.validate(),
            Err(ValidationError::DuplicateFunctionParameter(
                FunctionId(0),
                parameter.id
            ))
        );
    }

    #[test]
    fn binding_refresh_is_limited_to_local_lexical_cells() {
        let binding = Binding {
            id: BindingId(0),
            name: "iteration".into(),
            kind: BindingKind::Var,
            mutable: true,
        };
        let mut module = valid_module();
        module.functions[0].bindings.push(binding.clone());
        module.functions[0].blocks[0].instructions.insert(
            3,
            Instruction::RefreshBinding {
                binding: binding.id,
            },
        );
        assert_eq!(
            module.validate(),
            Err(ValidationError::InvalidRefreshBinding(
                FunctionId(0),
                binding.id
            ))
        );

        module.functions[0].bindings[0].kind = BindingKind::Let;
        assert_eq!(module.validate(), Ok(()));
    }
}
