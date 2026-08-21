//! Small, capability-neutral W3IR interpreter.
//!
//! The VM owns control flow and lexical frames only. JavaScript coercion,
//! objects, calls, construction and exceptions remain in `w3cos-core`, which
//! is also used by native AOT output.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::{Duration, Instant};

use w3cos_core::heap::{HeapOwner, HeapScope, HeapSnapshot};
use w3cos_core::{PanicValue, Value};
use w3cos_ir::{
    BinaryOperator, BindingId, BindingKind, BlockId, Constant, Function, FunctionId, Instruction,
    Module, Register, UnaryOperator, ValidationError,
};

/// Shared lexical storage used by module linking and closures. Imports point
/// at the exporting module's cell, preserving ESM live-binding semantics.
pub struct BindingSlot {
    value: RefCell<Value>,
    initialized: Cell<bool>,
    external_getter: Option<Value>,
    external_setter: Option<Value>,
}

impl BindingSlot {
    pub fn borrow(&self) -> Ref<'_, Value> {
        self.value.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, Value> {
        self.value.borrow_mut()
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.get()
    }

    /// Read the current cell value through either local storage or an
    /// external Core live-binding getter.
    pub fn read_value(&self) -> Value {
        match &self.external_getter {
            Some(getter) => getter.call(Value::Undefined, Vec::new()),
            None => self.borrow().clone(),
        }
    }

    fn read(&self, name: &str) -> Result<Value, VmError> {
        if !self.is_initialized() {
            return Err(VmError::ReferenceError(format!(
                "Cannot access '{name}' before initialization"
            )));
        }
        Ok(self.read_value())
    }

    fn initialize(&self, value: Value) {
        if let Some(setter) = &self.external_setter {
            setter.call(Value::Undefined, vec![value]);
            self.initialized.set(true);
            return;
        }
        *self.borrow_mut() = value;
        self.initialized.set(true);
    }

    fn store(&self, name: &str, value: Value) -> Result<(), VmError> {
        if self.external_getter.is_some() {
            let Some(setter) = &self.external_setter else {
                return Err(VmError::Thrown(w3cos_core::intrinsics::type_error(
                    &format!("Cannot assign to imported binding '{name}'"),
                )));
            };
            if !setter.is_callable() {
                return Err(VmError::Thrown(w3cos_core::intrinsics::type_error(
                    &format!("Cannot assign to imported binding '{name}'"),
                )));
            }
            setter.call(Value::Undefined, vec![value]);
            return Ok(());
        }
        if !self.is_initialized() {
            return Err(VmError::ReferenceError(format!(
                "Cannot access '{name}' before initialization"
            )));
        }
        *self.borrow_mut() = value;
        Ok(())
    }
}

pub type BindingCell = Rc<BindingSlot>;
pub type BindingCells = HashMap<BindingId, BindingCell>;
type Environment = BindingCells;
type DynamicImportHandler = Rc<dyn Fn(String) -> Value>;

pub fn binding_cell(value: Value) -> BindingCell {
    Rc::new(BindingSlot {
        value: RefCell::new(value),
        initialized: Cell::new(true),
        external_getter: None,
        external_setter: None,
    })
}

pub fn uninitialized_binding_cell() -> BindingCell {
    Rc::new(BindingSlot {
        value: RefCell::new(Value::Undefined),
        initialized: Cell::new(false),
        external_getter: None,
        external_setter: None,
    })
}

pub fn external_binding_cell(getter: Value, setter: Value) -> BindingCell {
    Rc::new(BindingSlot {
        value: RefCell::new(Value::Undefined),
        initialized: Cell::new(true),
        external_getter: Some(getter),
        external_setter: setter.is_callable().then_some(setter),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_instructions: u64,
    pub max_call_depth: u32,
    /// Estimated live Core heap bytes owned by this VM. The accounting path is
    /// shared with AOT and Host-created values; `None` disables the limit.
    pub max_heap_bytes: Option<usize>,
    /// Cumulative active wall time for one VM invocation. Time spent suspended
    /// at `await`/`yield` boundaries is excluded; `None` disables the limit.
    pub max_wall_time: Option<Duration>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_instructions: 1_000_000,
            max_call_depth: 512,
            max_heap_bytes: Some(64 * 1024 * 1024),
            max_wall_time: Some(Duration::from_secs(5)),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Rc<Cell<bool>>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.set(true);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}

#[derive(Debug)]
pub enum VmError {
    InvalidModule(ValidationError),
    MissingFunction(FunctionId),
    MissingBlock(FunctionId, BlockId),
    MissingBinding(BindingId),
    ReferenceError(String),
    InstructionLimitExceeded,
    CallDepthExceeded,
    HeapLimitExceeded { used: usize, limit: usize },
    WallClockLimitExceeded,
    Cancelled,
    Unsupported(&'static str),
    Thrown(Value),
    HostPanic,
}

impl std::fmt::Display for VmError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for VmError {}

#[derive(Clone)]
pub struct Vm(Rc<VmInner>);

struct VmInner {
    module: Module,
    limits: Limits,
    cancellation: CancellationToken,
    instructions: Cell<u64>,
    call_depth: Cell<u32>,
    execution_budget: RefCell<ExecutionBudget>,
    heap_owner: HeapOwner,
    dynamic_import: RefCell<Option<DynamicImportHandler>>,
}

struct ExecutionBudget {
    remaining: Option<Duration>,
    active_since: Option<Instant>,
    nesting: u32,
}

impl ExecutionBudget {
    fn new(limit: Option<Duration>) -> Self {
        Self {
            remaining: limit,
            active_since: None,
            nesting: 0,
        }
    }

    fn reset(&mut self, limit: Option<Duration>) {
        self.remaining = limit;
        self.active_since = None;
        self.nesting = 0;
    }
}

struct ExecutionSegment {
    inner: Rc<VmInner>,
    _heap_scope: HeapScope,
}

impl Drop for ExecutionSegment {
    fn drop(&mut self) {
        let mut budget = self.inner.execution_budget.borrow_mut();
        budget.nesting = budget
            .nesting
            .checked_sub(1)
            .expect("execution segment nesting is balanced");
        if budget.nesting != 0 {
            return;
        }
        if let (Some(remaining), Some(started)) = (budget.remaining, budget.active_since.take()) {
            budget.remaining = Some(remaining.saturating_sub(started.elapsed()));
        }
    }
}

struct AsyncFrame {
    function: Function,
    registers: Vec<Value>,
    environment: Environment,
    block: BlockId,
    depth: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GeneratorState {
    SuspendedStart,
    SuspendedYield(w3cos_ir::SuspensionId),
    SuspendedDelegate(w3cos_ir::SuspensionId),
    Executing,
    Completed,
}

struct GeneratorFrame {
    state: Cell<GeneratorState>,
    data: RefCell<GeneratorFrameData>,
}

struct GeneratorFrameData {
    function: Function,
    registers: Vec<Value>,
    environment: Environment,
    block: BlockId,
    depth: u32,
    delegate_iterator: Option<Value>,
}

struct AsyncGeneratorFrame {
    generator: Rc<GeneratorFrame>,
    queue: RefCell<VecDeque<AsyncGeneratorRequest>>,
    active: RefCell<Option<AsyncGeneratorRequest>>,
}

struct AsyncGeneratorRequest {
    kind: GeneratorResumeKind,
    input: Value,
    resolve: Value,
    reject: Value,
}

#[derive(Clone, Copy)]
enum GeneratorResumeKind {
    Next,
    Return,
    Throw,
}

#[derive(Clone, Copy)]
enum AsyncDelegateAction {
    Next,
    Return,
    Throw,
    MissingThrowClose,
}

enum GeneratorOutcome {
    Yielded(w3cos_ir::SuspensionId, Value),
    Delegated(w3cos_ir::SuspensionId, Value),
    Awaited(w3cos_ir::SuspensionId, Register, Value),
    DelegateAwaited(w3cos_ir::SuspensionId, AsyncDelegateAction, Value),
    Completed(Value),
}

impl Vm {
    pub fn new(module: Module, limits: Limits) -> Result<Self, VmError> {
        module.validate().map_err(VmError::InvalidModule)?;
        Ok(Self(Rc::new(VmInner {
            module,
            limits,
            cancellation: CancellationToken::default(),
            instructions: Cell::new(0),
            call_depth: Cell::new(0),
            execution_budget: RefCell::new(ExecutionBudget::new(limits.max_wall_time)),
            heap_owner: HeapOwner::new(),
            dynamic_import: RefCell::new(None),
        })))
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.0.cancellation.clone()
    }

    /// Returns this VM/page's shared Core heap counters.
    pub fn heap_snapshot(&self) -> HeapSnapshot {
        self.0.heap_owner.snapshot()
    }

    pub fn set_dynamic_import_handler(&self, handler: impl Fn(String) -> Value + 'static) {
        *self.0.dynamic_import.borrow_mut() = Some(Rc::new(handler));
    }

    pub fn run(&self) -> Result<Value, VmError> {
        self.run_with_bindings(HashMap::new())
    }

    /// Executes a module entry with host-provided import/global lexical cells.
    pub fn run_with_bindings(&self, bindings: HashMap<BindingId, Value>) -> Result<Value, VmError> {
        self.run_with_cells(
            bindings
                .into_iter()
                .map(|(id, value)| (id, binding_cell(value)))
                .collect(),
        )
    }

    /// Executes a module entry with pre-linked lexical cells. Module loaders
    /// use this to alias imports directly to exporter storage.
    pub fn run_with_cells(&self, bindings: BindingCells) -> Result<Value, VmError> {
        self.0.instructions.set(0);
        self.0.call_depth.set(0);
        self.reset_execution_budget();
        self.invoke_with_cells(self.0.module.entry, Value::Undefined, Vec::new(), bindings)
    }

    /// Exposes bytecode through the same callable `Value` ABI as AOT and host
    /// functions, allowing calls in both directions without an adapter type.
    pub fn callable(
        &self,
        function: FunctionId,
        bindings: HashMap<BindingId, Value>,
    ) -> Result<Value, VmError> {
        if !self
            .0
            .module
            .functions
            .iter()
            .any(|candidate| candidate.id == function)
        {
            return Err(VmError::MissingFunction(function));
        }
        let vm = self.clone();
        Ok(Value::function(move |this_value, arguments| {
            match vm.invoke(function, this_value, arguments, bindings.clone()) {
                Ok(value) => value,
                Err(VmError::Thrown(value)) => w3cos_core::throw_value(value),
                Err(error) => w3cos_core::throw_value(Value::string(&error.to_string())),
            }
        }))
    }

    fn invoke(
        &self,
        function: FunctionId,
        this_value: Value,
        arguments: Vec<Value>,
        bindings: HashMap<BindingId, Value>,
    ) -> Result<Value, VmError> {
        self.0.instructions.set(0);
        self.0.call_depth.set(0);
        self.reset_execution_budget();
        let environment = bindings
            .into_iter()
            .map(|(id, value)| (id, binding_cell(value)))
            .collect();
        self.invoke_with_cells(function, this_value, arguments, environment)
    }

    fn invoke_with_cells(
        &self,
        function: FunctionId,
        this_value: Value,
        arguments: Vec<Value>,
        environment: Environment,
    ) -> Result<Value, VmError> {
        self.execute_function(function, this_value, arguments, environment)
    }

    fn execute_function(
        &self,
        function_id: FunctionId,
        this_value: Value,
        arguments: Vec<Value>,
        captures: Environment,
    ) -> Result<Value, VmError> {
        let _heap_scope = self.0.heap_owner.enter();
        let function = self
            .0
            .module
            .functions
            .iter()
            .find(|function| function.id == function_id)
            .cloned()
            .ok_or(VmError::MissingFunction(function_id))?;

        let depth = self.0.call_depth.get();
        if depth >= self.0.limits.max_call_depth {
            return Err(VmError::CallDepthExceeded);
        }
        let result = if function.is_generator {
            self.execute_generator_function(function, this_value, arguments, captures, depth)
        } else if function.is_async {
            self.execute_async_function(function, this_value, arguments, captures, depth)
        } else {
            self.0.call_depth.set(depth + 1);
            let result = self.execute_frame(&function, this_value, arguments, captures);
            self.0.call_depth.set(depth);
            result
        };
        if result.is_ok() {
            self.check_heap_limit()?;
        }
        result
    }

    fn execute_generator_function(
        &self,
        function: Function,
        this_value: Value,
        arguments: Vec<Value>,
        captures: Environment,
        depth: u32,
    ) -> Result<Value, VmError> {
        if function.is_async {
            return self.execute_async_generator_function(
                function, this_value, arguments, captures, depth,
            );
        }
        let environment = initialize_environment(&function, this_value, arguments, captures)?;
        let entry = function.entry;
        let registers = function.registers;
        let frame = Rc::new(GeneratorFrame {
            state: Cell::new(GeneratorState::SuspendedStart),
            data: RefCell::new(GeneratorFrameData {
                function,
                registers: vec![Value::Undefined; registers as usize],
                environment,
                block: entry,
                depth,
                delegate_iterator: None,
            }),
        });
        let generator = Value::object(HashMap::new());

        let next_vm = self.clone();
        let next_frame = Rc::clone(&frame);
        generator.set_property(
            "next",
            Value::function(move |_, arguments| {
                let input = arguments.first().cloned().unwrap_or(Value::Undefined);
                generator_resume(
                    &next_vm,
                    Rc::clone(&next_frame),
                    GeneratorResumeKind::Next,
                    input,
                )
            }),
        );

        let return_vm = self.clone();
        let return_frame = Rc::clone(&frame);
        generator.set_property(
            "return",
            Value::function(move |_, arguments| {
                let input = arguments.first().cloned().unwrap_or(Value::Undefined);
                generator_resume(
                    &return_vm,
                    Rc::clone(&return_frame),
                    GeneratorResumeKind::Return,
                    input,
                )
            }),
        );

        let throw_vm = self.clone();
        let throw_frame = Rc::clone(&frame);
        generator.set_property(
            "throw",
            Value::function(move |_, arguments| {
                let input = arguments.first().cloned().unwrap_or(Value::Undefined);
                generator_resume(
                    &throw_vm,
                    Rc::clone(&throw_frame),
                    GeneratorResumeKind::Throw,
                    input,
                )
            }),
        );

        let iterable = generator.clone();
        generator.set_property(
            "__w3cos_symbol_iterator",
            Value::function(move |_, _| iterable.clone()),
        );
        Ok(generator)
    }

    fn execute_async_generator_function(
        &self,
        function: Function,
        this_value: Value,
        arguments: Vec<Value>,
        captures: Environment,
        depth: u32,
    ) -> Result<Value, VmError> {
        let environment = initialize_environment(&function, this_value, arguments, captures)?;
        let entry = function.entry;
        let registers = function.registers;
        let generator_frame = Rc::new(GeneratorFrame {
            state: Cell::new(GeneratorState::SuspendedStart),
            data: RefCell::new(GeneratorFrameData {
                function,
                registers: vec![Value::Undefined; registers as usize],
                environment,
                block: entry,
                depth,
                delegate_iterator: None,
            }),
        });
        let frame = Rc::new(AsyncGeneratorFrame {
            generator: generator_frame,
            queue: RefCell::new(VecDeque::new()),
            active: RefCell::new(None),
        });
        let generator = Value::object(HashMap::new());

        for (name, kind) in [
            ("next", GeneratorResumeKind::Next),
            ("return", GeneratorResumeKind::Return),
            ("throw", GeneratorResumeKind::Throw),
        ] {
            let vm = self.clone();
            let frame = Rc::clone(&frame);
            generator.set_property(
                name,
                Value::function(move |_, arguments| {
                    let input = arguments.first().cloned().unwrap_or(Value::Undefined);
                    async_generator_enqueue(&vm, Rc::clone(&frame), kind, input)
                }),
            );
        }

        let iterable = generator.clone();
        generator.set_property(
            "__w3cos_symbol_async_iterator",
            Value::function(move |_, _| iterable.clone()),
        );
        Ok(generator)
    }

    fn drive_generator_frame(
        &self,
        frame: Rc<GeneratorFrame>,
    ) -> Result<GeneratorOutcome, VmError> {
        let _segment = self.enter_execution_segment();
        let depth = frame.data.borrow().depth;
        let previous_depth = self.0.call_depth.replace(depth + 1);
        let result = self.drive_generator_segment(Rc::clone(&frame));
        self.0.call_depth.set(previous_depth);
        result
    }

    fn drive_generator_segment(
        &self,
        frame: Rc<GeneratorFrame>,
    ) -> Result<GeneratorOutcome, VmError> {
        loop {
            let (function, block) = {
                let frame = frame.data.borrow();
                (frame.function.clone(), frame.block)
            };
            let current = function
                .blocks
                .iter()
                .find(|candidate| candidate.id == block)
                .cloned()
                .ok_or(VmError::MissingBlock(function.id, block))?;
            let mut next_block = None;

            for instruction in &current.instructions {
                self.consume_instruction()?;
                if let Instruction::Await {
                    dst,
                    value,
                    suspension,
                } = instruction
                {
                    let awaited = frame.data.borrow().registers[value.0 as usize].clone();
                    return Ok(GeneratorOutcome::Awaited(*suspension, *dst, awaited));
                }
                if let Instruction::Yield {
                    value, suspension, ..
                } = instruction
                {
                    let yielded = frame.data.borrow().registers[value.0 as usize].clone();
                    return Ok(GeneratorOutcome::Yielded(*suspension, yielded));
                }
                if let Instruction::YieldDelegate {
                    dst,
                    iterator,
                    suspension,
                } = instruction
                {
                    let iterator = frame.data.borrow().registers[iterator.0 as usize].clone();
                    if function.is_async {
                        frame.data.borrow_mut().delegate_iterator = Some(iterator.clone());
                        let awaited =
                            generator_optional_iterator_call(&iterator, "next", Vec::new());
                        let awaited = match awaited {
                            Ok(Some(awaited)) => awaited,
                            Ok(None) => {
                                let value = Value::string(
                                    "TypeError: delegated iterator has no next method",
                                );
                                if let Some(region) = function
                                    .exception_regions
                                    .iter()
                                    .find(|region| region.protected_blocks.contains(&current.id))
                                {
                                    let mut data = frame.data.borrow_mut();
                                    data.registers[region.exception.0 as usize] = value;
                                    next_block = region.catch_block.or(region.finally_block);
                                    break;
                                }
                                return Err(VmError::Thrown(value));
                            }
                            Err(VmError::Thrown(value)) => {
                                if let Some(region) = function
                                    .exception_regions
                                    .iter()
                                    .find(|region| region.protected_blocks.contains(&current.id))
                                {
                                    let mut data = frame.data.borrow_mut();
                                    data.registers[region.exception.0 as usize] = value;
                                    next_block = region.catch_block.or(region.finally_block);
                                    break;
                                }
                                return Err(VmError::Thrown(value));
                            }
                            Err(error) => return Err(error),
                        };
                        return Ok(GeneratorOutcome::DelegateAwaited(
                            *suspension,
                            AsyncDelegateAction::Next,
                            awaited,
                        ));
                    }
                    match generator_iterator_step(&iterator, "next", Vec::new()) {
                        Ok((value, true)) => {
                            let point = function
                                .generator_suspension_points
                                .iter()
                                .find(|point| point.id == *suspension)
                                .ok_or(VmError::Unsupported(
                                    "missing generator delegation point",
                                ))?;
                            let mut data = frame.data.borrow_mut();
                            data.registers[dst.0 as usize] = value;
                            next_block = Some(point.resume_block);
                            break;
                        }
                        Ok((value, false)) => {
                            frame.data.borrow_mut().delegate_iterator = Some(iterator);
                            return Ok(GeneratorOutcome::Delegated(*suspension, value));
                        }
                        Err(VmError::Thrown(value)) => {
                            if let Some(region) = function
                                .exception_regions
                                .iter()
                                .find(|region| region.protected_blocks.contains(&current.id))
                            {
                                let mut data = frame.data.borrow_mut();
                                data.registers[region.exception.0 as usize] = value;
                                next_block = region.catch_block.or(region.finally_block);
                                break;
                            }
                            return Err(VmError::Thrown(value));
                        }
                        Err(error) => return Err(error),
                    }
                }

                let outcome = {
                    let mut frame = frame.data.borrow_mut();
                    let GeneratorFrameData {
                        registers,
                        environment,
                        ..
                    } = &mut *frame;
                    catch_unwind(AssertUnwindSafe(|| {
                        self.execute_instruction(&function, instruction, registers, environment)
                    }))
                };
                let control = match outcome {
                    Ok(Ok(control)) => {
                        self.check_heap_limit()?;
                        control
                    }
                    Ok(Err(error))
                        if function
                            .exception_regions
                            .iter()
                            .any(|region| region.protected_blocks.contains(&current.id)) =>
                    {
                        execution_error_as_throw(error)?
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(payload) => {
                        if let Some(value) = payload.downcast_ref::<PanicValue>() {
                            Control::Throw(value.0.clone())
                        } else {
                            return Err(VmError::HostPanic);
                        }
                    }
                };

                match control {
                    Control::Continue => {}
                    Control::Jump(target) => {
                        next_block = Some(target);
                        break;
                    }
                    Control::Return(value) => return Ok(GeneratorOutcome::Completed(value)),
                    Control::Throw(value) => {
                        if let Some(region) = function
                            .exception_regions
                            .iter()
                            .find(|region| region.protected_blocks.contains(&current.id))
                        {
                            let mut data = frame.data.borrow_mut();
                            data.registers[region.exception.0 as usize] = value;
                            next_block = region.catch_block.or(region.finally_block);
                            break;
                        }
                        return Err(VmError::Thrown(value));
                    }
                }
            }

            frame.data.borrow_mut().block =
                next_block.ok_or(VmError::MissingBlock(function.id, current.id))?;
        }
    }

    fn prepare_generator_resume(
        &self,
        frame: &Rc<GeneratorFrame>,
        kind: GeneratorResumeKind,
        input: Value,
    ) -> Result<Option<GeneratorOutcome>, VmError> {
        match frame.state.get() {
            GeneratorState::Executing => {
                return Err(VmError::Thrown(Value::string(
                    "TypeError: generator is already executing",
                )));
            }
            GeneratorState::Completed => {
                return match kind {
                    GeneratorResumeKind::Next => {
                        Ok(Some(GeneratorOutcome::Completed(Value::Undefined)))
                    }
                    GeneratorResumeKind::Return => Ok(Some(GeneratorOutcome::Completed(input))),
                    GeneratorResumeKind::Throw => Err(VmError::Thrown(input)),
                };
            }
            GeneratorState::SuspendedStart => match kind {
                GeneratorResumeKind::Next => {}
                GeneratorResumeKind::Return => {
                    frame.state.set(GeneratorState::Completed);
                    return Ok(Some(GeneratorOutcome::Completed(input)));
                }
                GeneratorResumeKind::Throw => {
                    frame.state.set(GeneratorState::Completed);
                    return Err(VmError::Thrown(input));
                }
            },
            GeneratorState::SuspendedYield(suspension) => {
                let point = frame
                    .data
                    .borrow()
                    .function
                    .generator_suspension_points
                    .iter()
                    .find(|point| point.id == suspension)
                    .cloned()
                    .ok_or(VmError::Unsupported("missing generator suspension point"))?;
                let mut data = frame.data.borrow_mut();
                data.registers[point.result.0 as usize] = input;
                data.block = match kind {
                    GeneratorResumeKind::Next => point.resume_block,
                    GeneratorResumeKind::Return => point.return_block,
                    GeneratorResumeKind::Throw => point.throw_block,
                };
            }
            GeneratorState::SuspendedDelegate(suspension) => {
                let (point, iterator) = {
                    let data = frame.data.borrow();
                    let point = data
                        .function
                        .generator_suspension_points
                        .iter()
                        .find(|point| point.id == suspension)
                        .cloned()
                        .ok_or(VmError::Unsupported("missing generator delegation point"))?;
                    let iterator = data
                        .delegate_iterator
                        .clone()
                        .ok_or(VmError::Unsupported("missing delegated iterator"))?;
                    (point, iterator)
                };
                frame.state.set(GeneratorState::Executing);
                if frame.data.borrow().function.is_async {
                    return self.prepare_async_delegate_resume(frame, point, iterator, kind, input);
                }

                let delegated = match kind {
                    GeneratorResumeKind::Next => Some(generator_optional_iterator_step(
                        &iterator,
                        "next",
                        vec![input.clone()],
                    )),
                    GeneratorResumeKind::Return => Some(generator_optional_iterator_step(
                        &iterator,
                        "return",
                        vec![input.clone()],
                    )),
                    GeneratorResumeKind::Throw => {
                        let result = generator_optional_iterator_step(
                            &iterator,
                            "throw",
                            vec![input.clone()],
                        );
                        if matches!(result, Ok(None)) {
                            if let Err(error) =
                                generator_optional_iterator_step(&iterator, "return", Vec::new())
                            {
                                match error {
                                    VmError::Thrown(value) => {
                                        inject_generator_completion(
                                            frame,
                                            &point,
                                            value,
                                            point.throw_block,
                                        );
                                    }
                                    error => {
                                        frame.state.set(GeneratorState::Completed);
                                        return Err(error);
                                    }
                                }
                            } else {
                                inject_generator_completion(
                                    frame,
                                    &point,
                                    Value::string(
                                        "TypeError: delegated iterator has no throw method",
                                    ),
                                    point.throw_block,
                                );
                            }
                            None
                        } else {
                            Some(result)
                        }
                    }
                };

                match delegated {
                    None => {}
                    Some(Ok(None)) if matches!(kind, GeneratorResumeKind::Return) => {
                        inject_generator_completion(frame, &point, input, point.return_block);
                    }
                    Some(Ok(None)) => {
                        inject_generator_completion(
                            frame,
                            &point,
                            Value::string("TypeError: delegated iterator method is missing"),
                            point.throw_block,
                        );
                    }
                    Some(Ok(Some((value, false)))) => {
                        frame
                            .state
                            .set(GeneratorState::SuspendedDelegate(suspension));
                        return Ok(Some(GeneratorOutcome::Delegated(suspension, value)));
                    }
                    Some(Ok(Some((value, true)))) => {
                        let target = if matches!(kind, GeneratorResumeKind::Return) {
                            point.return_block
                        } else {
                            point.resume_block
                        };
                        inject_generator_completion(frame, &point, value, target);
                    }
                    Some(Err(VmError::Thrown(value))) => {
                        inject_generator_completion(frame, &point, value, point.throw_block);
                    }
                    Some(Err(error)) => {
                        frame.state.set(GeneratorState::Completed);
                        return Err(error);
                    }
                }
            }
        }

        frame.state.set(GeneratorState::Executing);
        Ok(None)
    }

    fn prepare_async_delegate_resume(
        &self,
        frame: &Rc<GeneratorFrame>,
        point: w3cos_ir::GeneratorSuspensionPoint,
        iterator: Value,
        kind: GeneratorResumeKind,
        input: Value,
    ) -> Result<Option<GeneratorOutcome>, VmError> {
        let (method, action) = match kind {
            GeneratorResumeKind::Next => ("next", AsyncDelegateAction::Next),
            GeneratorResumeKind::Return => ("return", AsyncDelegateAction::Return),
            GeneratorResumeKind::Throw => ("throw", AsyncDelegateAction::Throw),
        };
        let called = generator_optional_iterator_call(&iterator, method, vec![input.clone()]);
        let called = match called {
            Ok(called) => called,
            Err(VmError::Thrown(value)) => {
                inject_generator_completion(frame, &point, value, point.throw_block);
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if let Some(awaited) = called {
            return Ok(Some(GeneratorOutcome::DelegateAwaited(
                point.id, action, awaited,
            )));
        }

        match kind {
            GeneratorResumeKind::Return => {
                inject_generator_completion(frame, &point, input, point.return_block);
            }
            GeneratorResumeKind::Throw => {
                let close = generator_optional_iterator_call(&iterator, "return", Vec::new());
                match close {
                    Ok(Some(awaited)) => {
                        return Ok(Some(GeneratorOutcome::DelegateAwaited(
                            point.id,
                            AsyncDelegateAction::MissingThrowClose,
                            awaited,
                        )));
                    }
                    Ok(None) => inject_generator_completion(
                        frame,
                        &point,
                        Value::string("TypeError: delegated iterator has no throw method"),
                        point.throw_block,
                    ),
                    Err(VmError::Thrown(value)) => {
                        inject_generator_completion(frame, &point, value, point.throw_block);
                    }
                    Err(error) => return Err(error),
                }
            }
            GeneratorResumeKind::Next => inject_generator_completion(
                frame,
                &point,
                Value::string("TypeError: delegated iterator has no next method"),
                point.throw_block,
            ),
        }
        Ok(None)
    }

    fn resume_generator_frame(
        &self,
        frame: Rc<GeneratorFrame>,
        kind: GeneratorResumeKind,
        input: Value,
    ) -> Result<Value, VmError> {
        let outcome = match self.prepare_generator_resume(&frame, kind, input)? {
            Some(outcome) => Ok(outcome),
            None => self.drive_generator_frame(Rc::clone(&frame)),
        };
        match outcome {
            Ok(GeneratorOutcome::Yielded(suspension, value)) => {
                frame.state.set(GeneratorState::SuspendedYield(suspension));
                Ok(generator_result(value, false))
            }
            Ok(GeneratorOutcome::Delegated(suspension, value)) => {
                frame
                    .state
                    .set(GeneratorState::SuspendedDelegate(suspension));
                Ok(generator_result(value, false))
            }
            Ok(GeneratorOutcome::Awaited(..)) => {
                frame.state.set(GeneratorState::Completed);
                Err(VmError::Unsupported("await in synchronous generator"))
            }
            Ok(GeneratorOutcome::DelegateAwaited(..)) => {
                frame.state.set(GeneratorState::Completed);
                Err(VmError::Unsupported(
                    "asynchronous delegation in synchronous generator",
                ))
            }
            Ok(GeneratorOutcome::Completed(value)) => {
                frame.state.set(GeneratorState::Completed);
                Ok(generator_result(value, true))
            }
            Err(error) => {
                frame.state.set(GeneratorState::Completed);
                Err(error)
            }
        }
    }

    fn drive_async_generator_queue(&self, frame: Rc<AsyncGeneratorFrame>) {
        if frame.active.borrow().is_some() {
            return;
        }
        let Some(request) = frame.queue.borrow_mut().pop_front() else {
            return;
        };
        let kind = request.kind;
        let input = request.input.clone();
        *frame.active.borrow_mut() = Some(request);

        let outcome = match self.prepare_generator_resume(&frame.generator, kind, input) {
            Ok(Some(outcome)) => Ok(outcome),
            Ok(None) => self.drive_generator_frame(Rc::clone(&frame.generator)),
            Err(error) => Err(error),
        };
        self.handle_async_generator_outcome(frame, outcome);
    }

    fn continue_async_generator(&self, frame: Rc<AsyncGeneratorFrame>) {
        let outcome = self.drive_generator_frame(Rc::clone(&frame.generator));
        self.handle_async_generator_outcome(frame, outcome);
    }

    fn handle_async_generator_outcome(
        &self,
        frame: Rc<AsyncGeneratorFrame>,
        outcome: Result<GeneratorOutcome, VmError>,
    ) {
        match outcome {
            Ok(GeneratorOutcome::Awaited(suspension, dst, awaited)) => {
                let point = frame
                    .generator
                    .data
                    .borrow()
                    .function
                    .suspension_points
                    .iter()
                    .find(|point| point.id == suspension)
                    .cloned();
                let Some(point) = point else {
                    self.reject_async_generator_request(
                        frame,
                        Value::string("missing async-generator await suspension point"),
                        true,
                    );
                    return;
                };

                let fulfilled_vm = self.clone();
                let fulfilled_frame = Rc::clone(&frame);
                let on_fulfilled = Value::function(move |_, arguments| {
                    {
                        let mut data = fulfilled_frame.generator.data.borrow_mut();
                        data.registers[dst.0 as usize] =
                            arguments.first().cloned().unwrap_or(Value::Undefined);
                        data.block = point.resume_block;
                    }
                    fulfilled_vm.continue_async_generator(Rc::clone(&fulfilled_frame));
                    Value::Undefined
                });

                let rejected_vm = self.clone();
                let rejected_frame = Rc::clone(&frame);
                let reject_block = point.reject_block;
                let on_rejected = Value::function(move |_, arguments| {
                    {
                        let mut data = rejected_frame.generator.data.borrow_mut();
                        data.registers[dst.0 as usize] =
                            arguments.first().cloned().unwrap_or(Value::Undefined);
                        data.block = reject_block;
                    }
                    rejected_vm.continue_async_generator(Rc::clone(&rejected_frame));
                    Value::Undefined
                });
                let awaited = w3cos_core::intrinsics::await_value(&awaited);
                w3cos_core::intrinsics::call_method(
                    &awaited,
                    &Value::string("then"),
                    vec![on_fulfilled, on_rejected],
                );
            }
            Ok(GeneratorOutcome::Yielded(suspension, yielded)) => {
                self.await_async_generator_yield(frame, suspension, yielded, false);
            }
            Ok(GeneratorOutcome::Delegated(suspension, yielded)) => {
                self.await_async_generator_yield(frame, suspension, yielded, true);
            }
            Ok(GeneratorOutcome::DelegateAwaited(suspension, action, awaited)) => {
                self.await_async_generator_delegate(frame, suspension, action, awaited);
            }
            Ok(GeneratorOutcome::Completed(value)) => {
                frame.generator.state.set(GeneratorState::Completed);
                self.await_async_generator_completion(frame, value);
            }
            Err(VmError::Thrown(value)) => {
                self.reject_async_generator_request(frame, value, true);
            }
            Err(error) => {
                self.reject_async_generator_request(frame, Value::string(&error.to_string()), true);
            }
        }
    }

    fn await_async_generator_yield(
        &self,
        frame: Rc<AsyncGeneratorFrame>,
        suspension: w3cos_ir::SuspensionId,
        yielded: Value,
        delegated: bool,
    ) {
        let point = frame
            .generator
            .data
            .borrow()
            .function
            .generator_suspension_points
            .iter()
            .find(|point| point.id == suspension)
            .cloned();
        let Some(point) = point else {
            self.reject_async_generator_request(
                frame,
                Value::string("missing async-generator yield suspension point"),
                true,
            );
            return;
        };

        let fulfilled_vm = self.clone();
        let fulfilled_frame = Rc::clone(&frame);
        let on_fulfilled = Value::function(move |_, arguments| {
            fulfilled_frame.generator.state.set(if delegated {
                GeneratorState::SuspendedDelegate(suspension)
            } else {
                GeneratorState::SuspendedYield(suspension)
            });
            let value = arguments.first().cloned().unwrap_or(Value::Undefined);
            fulfilled_vm.resolve_async_generator_request(
                Rc::clone(&fulfilled_frame),
                generator_result(value, false),
            );
            Value::Undefined
        });

        let rejected_vm = self.clone();
        let rejected_frame = Rc::clone(&frame);
        let on_rejected = Value::function(move |_, arguments| {
            let reason = arguments.first().cloned().unwrap_or(Value::Undefined);
            inject_generator_completion(
                &rejected_frame.generator,
                &point,
                reason,
                point.throw_block,
            );
            rejected_frame
                .generator
                .state
                .set(GeneratorState::Executing);
            rejected_vm.continue_async_generator(Rc::clone(&rejected_frame));
            Value::Undefined
        });
        let awaited = w3cos_core::intrinsics::await_value(&yielded);
        w3cos_core::intrinsics::call_method(
            &awaited,
            &Value::string("then"),
            vec![on_fulfilled, on_rejected],
        );
    }

    fn await_async_generator_delegate(
        &self,
        frame: Rc<AsyncGeneratorFrame>,
        suspension: w3cos_ir::SuspensionId,
        action: AsyncDelegateAction,
        awaited: Value,
    ) {
        let point = frame
            .generator
            .data
            .borrow()
            .function
            .generator_suspension_points
            .iter()
            .find(|point| point.id == suspension)
            .cloned();
        let Some(point) = point else {
            self.reject_async_generator_request(
                frame,
                Value::string("missing async-generator delegation point"),
                true,
            );
            return;
        };

        let fulfilled_vm = self.clone();
        let fulfilled_frame = Rc::clone(&frame);
        let fulfilled_point = point.clone();
        let on_fulfilled = Value::function(move |_, arguments| {
            let result = arguments.first().cloned().unwrap_or(Value::Undefined);
            if !result.is_object() && !result.is_function() {
                inject_generator_completion(
                    &fulfilled_frame.generator,
                    &fulfilled_point,
                    Value::string("TypeError: delegated iterator result is not an object"),
                    fulfilled_point.throw_block,
                );
                fulfilled_vm.continue_async_generator(Rc::clone(&fulfilled_frame));
                return Value::Undefined;
            }
            if matches!(action, AsyncDelegateAction::MissingThrowClose) {
                inject_generator_completion(
                    &fulfilled_frame.generator,
                    &fulfilled_point,
                    Value::string("TypeError: delegated iterator has no throw method"),
                    fulfilled_point.throw_block,
                );
                fulfilled_vm.continue_async_generator(Rc::clone(&fulfilled_frame));
                return Value::Undefined;
            }

            let value = result.get_property("value");
            if result.get_property("done").to_bool() {
                let target = if matches!(action, AsyncDelegateAction::Return) {
                    fulfilled_point.return_block
                } else {
                    fulfilled_point.resume_block
                };
                inject_generator_completion(
                    &fulfilled_frame.generator,
                    &fulfilled_point,
                    value,
                    target,
                );
                fulfilled_vm.continue_async_generator(Rc::clone(&fulfilled_frame));
            } else {
                fulfilled_vm.await_async_generator_yield(
                    Rc::clone(&fulfilled_frame),
                    suspension,
                    value,
                    true,
                );
            }
            Value::Undefined
        });

        let rejected_vm = self.clone();
        let rejected_frame = Rc::clone(&frame);
        let on_rejected = Value::function(move |_, arguments| {
            let reason = arguments.first().cloned().unwrap_or(Value::Undefined);
            inject_generator_completion(
                &rejected_frame.generator,
                &point,
                reason,
                point.throw_block,
            );
            rejected_vm.continue_async_generator(Rc::clone(&rejected_frame));
            Value::Undefined
        });
        let awaited = w3cos_core::intrinsics::await_value(&awaited);
        w3cos_core::intrinsics::call_method(
            &awaited,
            &Value::string("then"),
            vec![on_fulfilled, on_rejected],
        );
    }

    fn await_async_generator_completion(&self, frame: Rc<AsyncGeneratorFrame>, completion: Value) {
        let fulfilled_vm = self.clone();
        let fulfilled_frame = Rc::clone(&frame);
        let on_fulfilled = Value::function(move |_, arguments| {
            let value = arguments.first().cloned().unwrap_or(Value::Undefined);
            fulfilled_vm.resolve_async_generator_request(
                Rc::clone(&fulfilled_frame),
                generator_result(value, true),
            );
            Value::Undefined
        });
        let rejected_vm = self.clone();
        let rejected_frame = Rc::clone(&frame);
        let on_rejected = Value::function(move |_, arguments| {
            let reason = arguments.first().cloned().unwrap_or(Value::Undefined);
            rejected_vm.reject_async_generator_request(Rc::clone(&rejected_frame), reason, true);
            Value::Undefined
        });
        let awaited = w3cos_core::intrinsics::await_value(&completion);
        w3cos_core::intrinsics::call_method(
            &awaited,
            &Value::string("then"),
            vec![on_fulfilled, on_rejected],
        );
    }

    fn resolve_async_generator_request(&self, frame: Rc<AsyncGeneratorFrame>, value: Value) {
        let request = frame.active.borrow_mut().take();
        if let Some(request) = request {
            request.resolve.call(Value::Undefined, vec![value]);
        }
        self.drive_async_generator_queue(frame);
    }

    fn reject_async_generator_request(
        &self,
        frame: Rc<AsyncGeneratorFrame>,
        reason: Value,
        complete: bool,
    ) {
        if complete {
            frame.generator.state.set(GeneratorState::Completed);
        }
        let request = frame.active.borrow_mut().take();
        if let Some(request) = request {
            request.reject.call(Value::Undefined, vec![reason]);
        }
        self.drive_async_generator_queue(frame);
    }

    fn execute_async_function(
        &self,
        function: Function,
        this_value: Value,
        arguments: Vec<Value>,
        captures: Environment,
        depth: u32,
    ) -> Result<Value, VmError> {
        let environment = initialize_environment(&function, this_value, arguments, captures)?;
        let frame = Rc::new(RefCell::new(AsyncFrame {
            registers: vec![Value::Undefined; function.registers as usize],
            block: function.entry,
            function,
            environment,
            depth,
        }));
        let vm = self.clone();
        Ok(w3cos_core::intrinsics::promise_new(vec![Value::function(
            move |_, arguments| {
                let resolve = arguments.first().cloned().unwrap_or(Value::Undefined);
                let reject = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                vm.drive_async_frame(Rc::clone(&frame), resolve, reject);
                Value::Undefined
            },
        )]))
    }

    fn drive_async_frame(&self, frame: Rc<RefCell<AsyncFrame>>, resolve: Value, reject: Value) {
        let _segment = self.enter_execution_segment();
        let depth = frame.borrow().depth;
        let previous_depth = self.0.call_depth.replace(depth + 1);
        let result = self.drive_async_segment(Rc::clone(&frame), resolve.clone(), reject.clone());
        self.0.call_depth.set(previous_depth);
        match result {
            Ok(Some(value)) => {
                resolve.call(Value::Undefined, vec![value]);
            }
            Ok(None) => {}
            Err(VmError::Thrown(value)) => {
                reject.call(Value::Undefined, vec![value]);
            }
            Err(error) => {
                reject.call(Value::Undefined, vec![Value::string(&error.to_string())]);
            }
        }
    }

    fn drive_async_segment(
        &self,
        frame: Rc<RefCell<AsyncFrame>>,
        resolve: Value,
        reject: Value,
    ) -> Result<Option<Value>, VmError> {
        loop {
            let (function, block) = {
                let frame = frame.borrow();
                (frame.function.clone(), frame.block)
            };
            let current = function
                .blocks
                .iter()
                .find(|candidate| candidate.id == block)
                .cloned()
                .ok_or(VmError::MissingBlock(function.id, block))?;
            let mut next_block = None;

            for instruction in &current.instructions {
                self.consume_instruction()?;
                if let Instruction::Await {
                    dst,
                    value,
                    suspension,
                } = instruction
                {
                    let point = function
                        .suspension_points
                        .iter()
                        .find(|point| point.id == *suspension)
                        .cloned()
                        .ok_or(VmError::Unsupported("missing await suspension point"))?;
                    let awaited = frame.borrow().registers[value.0 as usize].clone();

                    let fulfilled_frame = Rc::clone(&frame);
                    let fulfilled_vm = self.clone();
                    let fulfilled_resolve = resolve.clone();
                    let fulfilled_reject = reject.clone();
                    let fulfilled_dst = *dst;
                    let on_fulfilled = Value::function(move |_, arguments| {
                        {
                            let mut frame = fulfilled_frame.borrow_mut();
                            frame.registers[fulfilled_dst.0 as usize] =
                                arguments.first().cloned().unwrap_or(Value::Undefined);
                            frame.block = point.resume_block;
                        }
                        fulfilled_vm.drive_async_frame(
                            Rc::clone(&fulfilled_frame),
                            fulfilled_resolve.clone(),
                            fulfilled_reject.clone(),
                        );
                        Value::Undefined
                    });

                    let rejected_frame = Rc::clone(&frame);
                    let rejected_vm = self.clone();
                    let rejected_resolve = resolve.clone();
                    let rejected_reject = reject.clone();
                    let rejected_dst = *dst;
                    let reject_block = point.reject_block;
                    let on_rejected = Value::function(move |_, arguments| {
                        {
                            let mut frame = rejected_frame.borrow_mut();
                            frame.registers[rejected_dst.0 as usize] =
                                arguments.first().cloned().unwrap_or(Value::Undefined);
                            frame.block = reject_block;
                        }
                        rejected_vm.drive_async_frame(
                            Rc::clone(&rejected_frame),
                            rejected_resolve.clone(),
                            rejected_reject.clone(),
                        );
                        Value::Undefined
                    });
                    let awaited = w3cos_core::intrinsics::await_value(&awaited);
                    w3cos_core::intrinsics::call_method(
                        &awaited,
                        &Value::string("then"),
                        vec![on_fulfilled, on_rejected],
                    );
                    return Ok(None);
                }

                let outcome = {
                    let mut frame = frame.borrow_mut();
                    let AsyncFrame {
                        registers,
                        environment,
                        ..
                    } = &mut *frame;
                    catch_unwind(AssertUnwindSafe(|| {
                        self.execute_instruction(&function, instruction, registers, environment)
                    }))
                };
                let control = match outcome {
                    Ok(Ok(control)) => {
                        self.check_heap_limit()?;
                        control
                    }
                    Ok(Err(error))
                        if function
                            .exception_regions
                            .iter()
                            .any(|region| region.protected_blocks.contains(&current.id)) =>
                    {
                        execution_error_as_throw(error)?
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(payload) => {
                        if let Some(value) = payload.downcast_ref::<PanicValue>() {
                            Control::Throw(value.0.clone())
                        } else {
                            return Err(VmError::HostPanic);
                        }
                    }
                };

                match control {
                    Control::Continue => {}
                    Control::Jump(target) => {
                        next_block = Some(target);
                        break;
                    }
                    Control::Return(value) => return Ok(Some(value)),
                    Control::Throw(value) => {
                        if let Some(region) = function
                            .exception_regions
                            .iter()
                            .find(|region| region.protected_blocks.contains(&current.id))
                        {
                            frame.borrow_mut().registers[region.exception.0 as usize] = value;
                            next_block = region.catch_block.or(region.finally_block);
                            break;
                        }
                        return Err(VmError::Thrown(value));
                    }
                }
            }

            frame.borrow_mut().block =
                next_block.ok_or(VmError::MissingBlock(function.id, current.id))?;
        }
    }

    fn execute_frame(
        &self,
        function: &Function,
        this_value: Value,
        arguments: Vec<Value>,
        mut environment: Environment,
    ) -> Result<Value, VmError> {
        let _segment = self.enter_execution_segment();
        environment = initialize_environment(function, this_value, arguments, environment)?;

        let mut registers = vec![Value::Undefined; function.registers as usize];
        let mut block = function.entry;
        loop {
            let current = function
                .blocks
                .iter()
                .find(|candidate| candidate.id == block)
                .ok_or(VmError::MissingBlock(function.id, block))?;
            let mut next_block = None;

            for instruction in &current.instructions {
                self.consume_instruction()?;
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    self.execute_instruction(
                        function,
                        instruction,
                        &mut registers,
                        &mut environment,
                    )
                }));
                let control = match outcome {
                    Ok(Ok(control)) => {
                        self.check_heap_limit()?;
                        control
                    }
                    Ok(Err(error))
                        if function
                            .exception_regions
                            .iter()
                            .any(|region| region.protected_blocks.contains(&current.id)) =>
                    {
                        execution_error_as_throw(error)?
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(payload) => {
                        if let Some(value) = payload.downcast_ref::<PanicValue>() {
                            Control::Throw(value.0.clone())
                        } else {
                            return Err(VmError::HostPanic);
                        }
                    }
                };

                match control {
                    Control::Continue => {}
                    Control::Jump(target) => {
                        next_block = Some(target);
                        break;
                    }
                    Control::Return(value) => return Ok(value),
                    Control::Throw(value) => {
                        if let Some(region) = function
                            .exception_regions
                            .iter()
                            .find(|region| region.protected_blocks.contains(&current.id))
                        {
                            registers[region.exception.0 as usize] = value;
                            next_block = region.catch_block.or(region.finally_block);
                            break;
                        }
                        return Err(VmError::Thrown(value));
                    }
                }
            }

            block = next_block.ok_or(VmError::MissingBlock(function.id, current.id))?;
        }
    }

    fn consume_instruction(&self) -> Result<(), VmError> {
        if self.0.cancellation.is_cancelled() {
            return Err(VmError::Cancelled);
        }
        self.check_heap_limit()?;
        let wall_time_exhausted = {
            let budget = self.0.execution_budget.borrow();
            match (budget.remaining, budget.active_since) {
                (Some(remaining), Some(started)) => started.elapsed() >= remaining,
                (Some(remaining), None) => remaining.is_zero(),
                (None, _) => false,
            }
        };
        if wall_time_exhausted {
            return Err(VmError::WallClockLimitExceeded);
        }
        let next = self.0.instructions.get().saturating_add(1);
        if next > self.0.limits.max_instructions {
            return Err(VmError::InstructionLimitExceeded);
        }
        self.0.instructions.set(next);
        Ok(())
    }

    fn check_heap_limit(&self) -> Result<(), VmError> {
        let Some(limit) = self.0.limits.max_heap_bytes else {
            return Ok(());
        };
        let used = self.0.heap_owner.snapshot().live_bytes;
        if used > limit {
            return Err(VmError::HeapLimitExceeded { used, limit });
        }
        Ok(())
    }

    fn reset_execution_budget(&self) {
        self.0
            .execution_budget
            .borrow_mut()
            .reset(self.0.limits.max_wall_time);
    }

    fn enter_execution_segment(&self) -> ExecutionSegment {
        let heap_scope = self.0.heap_owner.enter();
        let mut budget = self.0.execution_budget.borrow_mut();
        if budget.nesting == 0 {
            budget.active_since = Some(Instant::now());
        }
        budget.nesting = budget
            .nesting
            .checked_add(1)
            .expect("execution segment nesting overflow");
        drop(budget);
        ExecutionSegment {
            inner: Rc::clone(&self.0),
            _heap_scope: heap_scope,
        }
    }

    fn execute_instruction(
        &self,
        function: &Function,
        instruction: &Instruction,
        registers: &mut [Value],
        environment: &mut Environment,
    ) -> Result<Control, VmError> {
        let read = |register: Register| registers[register.0 as usize].clone();
        let binding_name = |binding: BindingId| {
            self.0
                .module
                .functions
                .iter()
                .flat_map(|function| &function.bindings)
                .find(|candidate| candidate.id == binding)
                .map(|binding| binding.name.clone())
                .unwrap_or_else(|| format!("<binding:{}>", binding.0))
        };
        match instruction {
            Instruction::LoadConstant { dst, value } => {
                registers[dst.0 as usize] = constant(value);
            }
            Instruction::Move { dst, src } => {
                registers[dst.0 as usize] = read(*src);
            }
            Instruction::LoadBinding { dst, binding } => {
                registers[dst.0 as usize] = environment
                    .get(binding)
                    .ok_or(VmError::MissingBinding(*binding))?
                    .read(&binding_name(*binding))?;
            }
            Instruction::InitializeBinding { binding, value } => {
                environment
                    .get(binding)
                    .ok_or(VmError::MissingBinding(*binding))?
                    .initialize(read(*value));
            }
            Instruction::StoreBinding { binding, value } => {
                environment
                    .get(binding)
                    .ok_or(VmError::MissingBinding(*binding))?
                    .store(&binding_name(*binding), read(*value))?;
            }
            Instruction::RefreshBinding { binding } => {
                let current = environment
                    .get(binding)
                    .ok_or(VmError::MissingBinding(*binding))?;
                let refreshed = if current.is_initialized() {
                    binding_cell(current.read(&binding_name(*binding))?)
                } else {
                    uninitialized_binding_cell()
                };
                environment.insert(*binding, refreshed);
            }
            Instruction::Add { dst, lhs, rhs } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::add(&read(*lhs), &read(*rhs));
            }
            Instruction::Binary {
                dst,
                operator,
                lhs,
                rhs,
            } => {
                let lhs = read(*lhs);
                let rhs = read(*rhs);
                registers[dst.0 as usize] = match operator {
                    BinaryOperator::Subtract => w3cos_core::intrinsics::subtract(&lhs, &rhs),
                    BinaryOperator::Multiply => w3cos_core::intrinsics::multiply(&lhs, &rhs),
                    BinaryOperator::Divide => w3cos_core::intrinsics::divide(&lhs, &rhs),
                    BinaryOperator::Remainder => w3cos_core::intrinsics::remainder(&lhs, &rhs),
                    BinaryOperator::Exponentiate => {
                        w3cos_core::intrinsics::exponentiate(&lhs, &rhs)
                    }
                    BinaryOperator::AbstractEqual => {
                        w3cos_core::intrinsics::abstract_equal(&lhs, &rhs)
                    }
                    BinaryOperator::AbstractNotEqual => w3cos_core::intrinsics::logical_not(
                        &w3cos_core::intrinsics::abstract_equal(&lhs, &rhs),
                    ),
                    BinaryOperator::StrictEqual => w3cos_core::intrinsics::strict_equal(&lhs, &rhs),
                    BinaryOperator::StrictNotEqual => w3cos_core::intrinsics::logical_not(
                        &w3cos_core::intrinsics::strict_equal(&lhs, &rhs),
                    ),
                    BinaryOperator::LessThan => w3cos_core::intrinsics::less_than(&lhs, &rhs),
                    BinaryOperator::LessThanOrEqual => {
                        w3cos_core::intrinsics::less_than_or_equal(&lhs, &rhs)
                    }
                    BinaryOperator::GreaterThan => w3cos_core::intrinsics::greater_than(&lhs, &rhs),
                    BinaryOperator::GreaterThanOrEqual => {
                        w3cos_core::intrinsics::greater_than_or_equal(&lhs, &rhs)
                    }
                    BinaryOperator::LeftShift => w3cos_core::intrinsics::left_shift(&lhs, &rhs),
                    BinaryOperator::SignedRightShift => {
                        w3cos_core::intrinsics::signed_right_shift(&lhs, &rhs)
                    }
                    BinaryOperator::UnsignedRightShift => {
                        w3cos_core::intrinsics::unsigned_right_shift(&lhs, &rhs)
                    }
                    BinaryOperator::BitwiseOr => w3cos_core::intrinsics::bitwise_or(&lhs, &rhs),
                    BinaryOperator::BitwiseXor => w3cos_core::intrinsics::bitwise_xor(&lhs, &rhs),
                    BinaryOperator::BitwiseAnd => w3cos_core::intrinsics::bitwise_and(&lhs, &rhs),
                    BinaryOperator::InstanceOf => w3cos_core::intrinsics::instance_of(&lhs, &rhs),
                    BinaryOperator::In => w3cos_core::intrinsics::in_operator(&lhs, &rhs),
                };
            }
            Instruction::Unary {
                dst,
                operator,
                value,
            } => {
                registers[dst.0 as usize] = match operator {
                    UnaryOperator::TypeOf => w3cos_core::intrinsics::type_of(&read(*value)),
                    UnaryOperator::Negate => w3cos_core::intrinsics::negate(&read(*value)),
                    UnaryOperator::BitwiseNot => w3cos_core::intrinsics::bitwise_not(&read(*value)),
                };
            }
            Instruction::GetProperty { dst, object, key } => {
                registers[dst.0 as usize] =
                    w3cos_core::intrinsics::get_property(&read(*object), &read(*key));
            }
            Instruction::DeleteProperty { dst, object, key } => {
                registers[dst.0 as usize] =
                    w3cos_core::intrinsics::delete_property(&read(*object), &read(*key));
            }
            Instruction::SetProperty { object, key, value } => {
                w3cos_core::intrinsics::set_property(&read(*object), &read(*key), read(*value));
            }
            Instruction::DefineField { object, key, value } => {
                w3cos_core::intrinsics::define_field(&read(*object), &read(*key), read(*value));
            }
            Instruction::DefinePrivate {
                object,
                brand,
                name,
                value,
            } => {
                w3cos_core::intrinsics::define_private(
                    &read(*object),
                    &read(*brand),
                    &read(*name),
                    read(*value),
                );
            }
            Instruction::GetPrivate {
                dst,
                object,
                brand,
                name,
            } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::get_private(
                    &read(*object),
                    &read(*brand),
                    &read(*name),
                );
            }
            Instruction::SetPrivate {
                object,
                brand,
                name,
                value,
            } => {
                w3cos_core::intrinsics::set_private(
                    &read(*object),
                    &read(*brand),
                    &read(*name),
                    read(*value),
                );
            }
            Instruction::HasPrivate { dst, object, brand } => {
                registers[dst.0 as usize] =
                    w3cos_core::intrinsics::has_private(&read(*object), &read(*brand));
            }
            Instruction::DefinePrivateMethod { brand, name, value } => {
                w3cos_core::intrinsics::define_private_method(
                    &read(*brand),
                    &read(*name),
                    read(*value),
                );
            }
            Instruction::DefinePrivateAccessor {
                brand,
                name,
                getter,
                setter,
            } => {
                w3cos_core::intrinsics::define_private_accessor(
                    &read(*brand),
                    &read(*name),
                    getter.map(&read),
                    setter.map(&read),
                );
            }
            Instruction::CreateObject { dst, properties } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::create_object(
                    properties
                        .iter()
                        .map(|(key, value)| (read(*key), read(*value)))
                        .collect(),
                );
            }
            Instruction::CopyDataProperties { target, source } => {
                w3cos_core::intrinsics::copy_data_properties(&read(*target), &read(*source));
            }
            Instruction::CreateArray { dst, elements } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::create_array(
                    elements.iter().map(|element| read(*element)).collect(),
                );
            }
            Instruction::AppendArrayElement { array, value } => {
                w3cos_core::intrinsics::append_array_element(&read(*array), read(*value));
            }
            Instruction::AppendIterable { array, iterable } => {
                w3cos_core::intrinsics::append_iterable(&read(*array), &read(*iterable));
            }
            Instruction::ArrayRest { dst, value, start } => {
                registers[dst.0 as usize] =
                    w3cos_core::intrinsics::array_rest(&read(*value), *start as usize);
            }
            Instruction::ObjectRest {
                dst,
                value,
                excluded,
            } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::object_rest(
                    &read(*value),
                    &excluded.iter().map(|key| read(*key)).collect::<Vec<_>>(),
                );
            }
            Instruction::CreateClosure {
                dst,
                function,
                captures,
            } => {
                let captured = captures
                    .iter()
                    .map(|binding| {
                        environment
                            .get(binding)
                            .cloned()
                            .map(|cell| (*binding, cell))
                            .ok_or(VmError::MissingBinding(*binding))
                    })
                    .collect::<Result<Environment, _>>()?;
                let vm = self.clone();
                let function = *function;
                registers[dst.0 as usize] = Value::function(move |this_value, arguments| match vm
                    .execute_function(function, this_value, arguments, captured.clone())
                {
                    Ok(value) => value,
                    Err(VmError::Thrown(value)) => w3cos_core::throw_value(value),
                    Err(error) => w3cos_core::throw_value(Value::string(&error.to_string())),
                });
            }
            Instruction::CreateClass {
                dst,
                constructor,
                super_class,
                initializer,
            } => {
                let constructor = read(*constructor);
                let super_class = super_class.map(&read);
                let initializer = initializer.map(&read).unwrap_or(Value::Undefined);
                registers[dst.0 as usize] = w3cos_core::intrinsics::create_class_with_initializer(
                    &constructor,
                    super_class.as_ref(),
                    &initializer,
                );
            }
            Instruction::Call {
                dst,
                callee,
                this_value,
                arguments,
            } => {
                let arguments = arguments.iter().map(|register| read(*register)).collect();
                registers[dst.0 as usize] =
                    w3cos_core::intrinsics::call(&read(*callee), read(*this_value), arguments);
            }
            Instruction::CallWithArguments {
                dst,
                callee,
                this_value,
                arguments,
            } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::call_with_arguments(
                    &read(*callee),
                    read(*this_value),
                    &read(*arguments),
                );
            }
            Instruction::CallMethod {
                dst,
                object,
                key,
                arguments,
            } => {
                let arguments = arguments.iter().map(|register| read(*register)).collect();
                registers[dst.0 as usize] =
                    w3cos_core::intrinsics::call_method(&read(*object), &read(*key), arguments);
            }
            Instruction::CallMethodWithArguments {
                dst,
                object,
                key,
                arguments,
            } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::call_method_with_arguments(
                    &read(*object),
                    &read(*key),
                    &read(*arguments),
                );
            }
            Instruction::Construct {
                dst,
                constructor,
                arguments,
            } => {
                let arguments = arguments.iter().map(|register| read(*register)).collect();
                registers[dst.0 as usize] =
                    w3cos_core::intrinsics::construct(&read(*constructor), arguments);
            }
            Instruction::ConstructWithArguments {
                dst,
                constructor,
                arguments,
            } => {
                registers[dst.0 as usize] = w3cos_core::intrinsics::construct_with_arguments(
                    &read(*constructor),
                    &read(*arguments),
                );
            }
            Instruction::DynamicImport { dst, specifier } => {
                let handler = self
                    .0
                    .dynamic_import
                    .borrow()
                    .clone()
                    .ok_or(VmError::Unsupported("DynamicImport"))?;
                registers[dst.0 as usize] = handler(read(*specifier).to_js_string());
            }
            Instruction::ImportMeta { dst } => {
                let value = Value::object(HashMap::from([(
                    "url".into(),
                    Value::string(&self.0.module.specifier),
                )]));
                registers[dst.0 as usize] = value;
            }
            Instruction::Await { .. } => return Err(VmError::Unsupported("Await")),
            Instruction::Yield { .. } => return Err(VmError::Unsupported("Yield")),
            Instruction::YieldDelegate { .. } => {
                return Err(VmError::Unsupported("YieldDelegate"));
            }
            Instruction::Jump { target } => return Ok(Control::Jump(*target)),
            Instruction::Branch {
                condition,
                then_block,
                else_block,
            } => {
                return Ok(Control::Jump(if read(*condition).to_bool() {
                    *then_block
                } else {
                    *else_block
                }));
            }
            Instruction::Return { value } => return Ok(Control::Return(read(*value))),
            Instruction::Throw { value } => return Ok(Control::Throw(read(*value))),
        }
        let _ = function;
        Ok(Control::Continue)
    }
}

fn generator_resume(
    vm: &Vm,
    frame: Rc<GeneratorFrame>,
    kind: GeneratorResumeKind,
    input: Value,
) -> Value {
    match vm.resume_generator_frame(frame, kind, input) {
        Ok(result) => result,
        Err(VmError::Thrown(value)) => w3cos_core::throw_value(value),
        Err(error) => w3cos_core::throw_value(Value::string(&error.to_string())),
    }
}

fn async_generator_enqueue(
    vm: &Vm,
    frame: Rc<AsyncGeneratorFrame>,
    kind: GeneratorResumeKind,
    input: Value,
) -> Value {
    let vm = vm.clone();
    w3cos_core::intrinsics::promise_new(vec![Value::function(move |_, arguments| {
        let resolve = arguments.first().cloned().unwrap_or(Value::Undefined);
        let reject = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        frame.queue.borrow_mut().push_back(AsyncGeneratorRequest {
            kind,
            input: input.clone(),
            resolve,
            reject,
        });
        vm.drive_async_generator_queue(Rc::clone(&frame));
        Value::Undefined
    })])
}

fn generator_result(value: Value, done: bool) -> Value {
    Value::object(HashMap::from([
        ("value".into(), value),
        ("done".into(), Value::Bool(done)),
    ]))
}

fn inject_generator_completion(
    frame: &Rc<GeneratorFrame>,
    point: &w3cos_ir::GeneratorSuspensionPoint,
    value: Value,
    target: BlockId,
) {
    let mut data = frame.data.borrow_mut();
    data.delegate_iterator = None;
    data.registers[point.result.0 as usize] = value;
    data.block = target;
}

fn generator_iterator_step(
    iterator: &Value,
    method: &str,
    arguments: Vec<Value>,
) -> Result<(Value, bool), VmError> {
    generator_optional_iterator_step(iterator, method, arguments)?.ok_or_else(|| {
        VmError::Thrown(Value::string(&format!(
            "TypeError: delegated iterator has no {method} method"
        )))
    })
}

fn generator_optional_iterator_step(
    iterator: &Value,
    method: &str,
    arguments: Vec<Value>,
) -> Result<Option<(Value, bool)>, VmError> {
    let method_value = iterator.get_property(method);
    if method_value.is_nullish() {
        return Ok(None);
    }
    if !method_value.is_callable() {
        return Err(VmError::Thrown(Value::string(&format!(
            "TypeError: delegated iterator {method} is not callable"
        ))));
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        method_value.call(iterator.clone(), arguments)
    }));
    let result = match result {
        Ok(result) => result,
        Err(payload) => {
            if let Some(value) = payload.downcast_ref::<PanicValue>() {
                return Err(VmError::Thrown(value.0.clone()));
            }
            return Err(VmError::HostPanic);
        }
    };
    if !result.is_object() && !result.is_function() {
        return Err(VmError::Thrown(Value::string(
            "TypeError: delegated iterator result is not an object",
        )));
    }
    Ok(Some((
        result.get_property("value"),
        result.get_property("done").to_bool(),
    )))
}

fn generator_optional_iterator_call(
    iterator: &Value,
    method: &str,
    arguments: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let method_value = iterator.get_property(method);
    if method_value.is_nullish() {
        return Ok(None);
    }
    if !method_value.is_callable() {
        return Err(VmError::Thrown(Value::string(&format!(
            "TypeError: delegated iterator {method} is not callable"
        ))));
    }
    match catch_unwind(AssertUnwindSafe(|| {
        method_value.call(iterator.clone(), arguments)
    })) {
        Ok(result) => Ok(Some(result)),
        Err(payload) => {
            if let Some(value) = payload.downcast_ref::<PanicValue>() {
                Err(VmError::Thrown(value.0.clone()))
            } else {
                Err(VmError::HostPanic)
            }
        }
    }
}

enum Control {
    Continue,
    Jump(BlockId),
    Return(Value),
    Throw(Value),
}

fn execution_error_as_throw(error: VmError) -> Result<Control, VmError> {
    match error {
        VmError::Thrown(value) => Ok(Control::Throw(value)),
        VmError::ReferenceError(message) => Ok(Control::Throw(
            w3cos_core::intrinsics::reference_error(&message),
        )),
        other => Err(other),
    }
}

fn initialize_environment(
    function: &Function,
    this_value: Value,
    arguments: Vec<Value>,
    mut environment: Environment,
) -> Result<Environment, VmError> {
    for binding in &function.bindings {
        environment.entry(binding.id).or_insert_with(|| {
            if matches!(
                binding.kind,
                BindingKind::Let
                    | BindingKind::Const
                    | BindingKind::Class
                    | BindingKind::Import
                    | BindingKind::Catch
            ) {
                uninitialized_binding_cell()
            } else {
                binding_cell(Value::Undefined)
            }
        });
    }
    for (binding, value) in function.parameters.iter().zip(arguments.iter().cloned()) {
        environment
            .get(binding)
            .ok_or(VmError::MissingBinding(*binding))?
            .initialize(value);
    }
    if let Some(binding) = function.arguments_binding {
        let value = w3cos_core::intrinsics::create_array(arguments.clone());
        environment
            .get(&binding)
            .ok_or(VmError::MissingBinding(binding))?
            .initialize(value);
    }
    if let Some(binding) = function.rest_parameter {
        let value = w3cos_core::intrinsics::create_array(
            arguments
                .into_iter()
                .skip(function.parameters.len())
                .collect(),
        );
        environment
            .get(&binding)
            .ok_or(VmError::MissingBinding(binding))?
            .initialize(value);
    }
    if let Some(binding) = function.this_binding {
        environment
            .get(&binding)
            .ok_or(VmError::MissingBinding(binding))?
            .initialize(this_value);
    }
    Ok(environment)
}

fn constant(value: &Constant) -> Value {
    match value {
        Constant::Undefined => Value::Undefined,
        Constant::Null => Value::Null,
        Constant::Bool(value) => Value::Bool(*value),
        Constant::Number(value) => Value::Number(*value),
        Constant::String(value) => Value::string(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_ir::{Binding, BindingKind, Block, ExceptionRegion, Import};

    #[test]
    fn async_execution_cannot_bypass_the_shared_promise_intrinsics() {
        let production = include_str!("lib.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source");
        for required in [
            "w3cos_core::intrinsics::await_value",
            "w3cos_core::intrinsics::promise_new",
            "w3cos_core::intrinsics::call_method",
        ] {
            assert!(production.contains(required), "missing {required}");
        }
        for bypass in [
            "w3cos_core::promise::resolve",
            "w3cos_core::promise::new",
            ".call_method(\"then\"",
        ] {
            assert!(
                !production.contains(bypass),
                "W3VM async execution bypasses the shared ABI via {bypass}"
            );
        }
    }

    fn block(id: u32, instructions: Vec<Instruction>) -> Block {
        Block {
            id: BlockId(id),
            instructions,
            source_span: None,
        }
    }

    fn function(
        id: u32,
        registers: u32,
        bindings: Vec<Binding>,
        captures: Vec<BindingId>,
        blocks: Vec<Block>,
    ) -> Function {
        Function {
            id: FunctionId(id),
            name: None,
            parameters: Vec::new(),
            rest_parameter: None,
            arguments_binding: None,
            bindings,
            captures,
            this_binding: None,
            registers,
            entry: BlockId(0),
            blocks,
            exception_regions: Vec::new(),
            suspension_points: Vec::new(),
            generator_suspension_points: Vec::new(),
            is_async: false,
            is_generator: false,
            source_span: None,
        }
    }

    #[test]
    fn executes_control_flow_through_shared_add_semantics() {
        let module = Module::new(
            "app:///main.js",
            FunctionId(0),
            vec![function(
                0,
                4,
                Vec::new(),
                Vec::new(),
                vec![
                    block(
                        0,
                        vec![
                            Instruction::LoadConstant {
                                dst: Register(0),
                                value: Constant::String("value:".into()),
                            },
                            Instruction::LoadConstant {
                                dst: Register(1),
                                value: Constant::Number(3.0),
                            },
                            Instruction::Add {
                                dst: Register(2),
                                lhs: Register(0),
                                rhs: Register(1),
                            },
                            Instruction::LoadConstant {
                                dst: Register(3),
                                value: Constant::Bool(true),
                            },
                            Instruction::Branch {
                                condition: Register(3),
                                then_block: BlockId(1),
                                else_block: BlockId(2),
                            },
                        ],
                    ),
                    block(1, vec![Instruction::Return { value: Register(2) }]),
                    block(2, vec![Instruction::Return { value: Register(1) }]),
                ],
            )],
        );
        assert_eq!(
            Vm::new(module, Limits::default()).unwrap().run().unwrap(),
            Value::string("value:3")
        );
    }

    #[test]
    fn bytecode_closure_captures_a_live_lexical_cell() {
        let binding = Binding {
            id: BindingId(0),
            name: "value".into(),
            kind: BindingKind::Let,
            mutable: true,
        };
        let inner = function(
            1,
            1,
            Vec::new(),
            vec![binding.id],
            vec![block(
                0,
                vec![
                    Instruction::LoadBinding {
                        dst: Register(0),
                        binding: binding.id,
                    },
                    Instruction::Return { value: Register(0) },
                ],
            )],
        );
        let outer = function(
            0,
            4,
            vec![binding.clone()],
            Vec::new(),
            vec![block(
                0,
                vec![
                    Instruction::LoadConstant {
                        dst: Register(0),
                        value: Constant::Number(7.0),
                    },
                    Instruction::InitializeBinding {
                        binding: binding.id,
                        value: Register(0),
                    },
                    Instruction::CreateClosure {
                        dst: Register(1),
                        function: inner.id,
                        captures: vec![binding.id],
                    },
                    Instruction::LoadConstant {
                        dst: Register(2),
                        value: Constant::Undefined,
                    },
                    Instruction::Call {
                        dst: Register(3),
                        callee: Register(1),
                        this_value: Register(2),
                        arguments: Vec::new(),
                    },
                    Instruction::Return { value: Register(3) },
                ],
            )],
        );
        let module = Module::new("app:///closure.js", outer.id, vec![outer, inner]);
        assert_eq!(
            Vm::new(module, Limits::default()).unwrap().run().unwrap(),
            Value::Number(7.0)
        );
    }

    #[test]
    fn lexical_cells_require_explicit_initialization_before_load_or_store() {
        let binding = Binding {
            id: BindingId(0),
            name: "value".into(),
            kind: BindingKind::Let,
            mutable: true,
        };
        let load_before_initialization = Module::new(
            "app:///tdz-load.js",
            FunctionId(0),
            vec![function(
                0,
                1,
                vec![binding.clone()],
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::LoadBinding {
                            dst: Register(0),
                            binding: binding.id,
                        },
                        Instruction::Return { value: Register(0) },
                    ],
                )],
            )],
        );
        assert!(matches!(
            Vm::new(load_before_initialization, Limits::default())
                .unwrap()
                .run(),
            Err(VmError::ReferenceError(message))
                if message == "Cannot access 'value' before initialization"
        ));

        let store_before_initialization = Module::new(
            "app:///tdz-store.js",
            FunctionId(0),
            vec![function(
                0,
                1,
                vec![binding.clone()],
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::LoadConstant {
                            dst: Register(0),
                            value: Constant::Number(1.0),
                        },
                        Instruction::StoreBinding {
                            binding: binding.id,
                            value: Register(0),
                        },
                        Instruction::Return { value: Register(0) },
                    ],
                )],
            )],
        );
        assert!(matches!(
            Vm::new(store_before_initialization, Limits::default())
                .unwrap()
                .run(),
            Err(VmError::ReferenceError(message))
                if message == "Cannot access 'value' before initialization"
        ));

        let initialized = Module::new(
            "app:///tdz-initialize.js",
            FunctionId(0),
            vec![function(
                0,
                2,
                vec![binding.clone()],
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::LoadConstant {
                            dst: Register(0),
                            value: Constant::Number(2.0),
                        },
                        Instruction::InitializeBinding {
                            binding: binding.id,
                            value: Register(0),
                        },
                        Instruction::LoadBinding {
                            dst: Register(1),
                            binding: binding.id,
                        },
                        Instruction::Return { value: Register(1) },
                    ],
                )],
            )],
        );
        assert_eq!(
            Vm::new(initialized, Limits::default())
                .unwrap()
                .run()
                .unwrap(),
            Value::Number(2.0)
        );
    }

    #[test]
    fn enforces_instruction_budget_and_cancellation() {
        let module = Module::new(
            "app:///loop.js",
            FunctionId(0),
            vec![function(
                0,
                0,
                Vec::new(),
                Vec::new(),
                vec![block(0, vec![Instruction::Jump { target: BlockId(0) }])],
            )],
        );
        let vm = Vm::new(
            module.clone(),
            Limits {
                max_instructions: 3,
                max_call_depth: 8,
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(matches!(vm.run(), Err(VmError::InstructionLimitExceeded)));

        let deadline = Vm::new(
            module.clone(),
            Limits {
                max_wall_time: Some(Duration::ZERO),
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(matches!(
            deadline.run(),
            Err(VmError::WallClockLimitExceeded)
        ));

        let cancelled = Vm::new(module, Limits::default()).unwrap();
        cancelled.cancellation_token().cancel();
        assert!(matches!(cancelled.run(), Err(VmError::Cancelled)));
    }

    #[test]
    fn enforces_shared_core_heap_budget_and_exposes_residency() {
        let module = Module::new(
            "app:///heap.js",
            FunctionId(0),
            vec![function(
                0,
                1,
                Vec::new(),
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::CreateObject {
                            dst: Register(0),
                            properties: Vec::new(),
                        },
                        Instruction::Return { value: Register(0) },
                    ],
                )],
            )],
        );

        let limited = Vm::new(
            module.clone(),
            Limits {
                max_heap_bytes: Some(1),
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(matches!(
            limited.run(),
            Err(VmError::HeapLimitExceeded { used, limit: 1 }) if used > 1
        ));

        let observed = Vm::new(
            module,
            Limits {
                max_heap_bytes: None,
                ..Limits::default()
            },
        )
        .unwrap();
        let retained = observed.run().unwrap();
        let snapshot = observed.heap_snapshot();
        assert!(snapshot.live_bytes > 0);
        assert_eq!(snapshot.live_objects, 1);
        drop(retained);
        assert_eq!(observed.heap_snapshot().live_objects, 0);
    }

    #[test]
    fn linked_module_bindings_share_live_lexical_cells() {
        let imported = Binding {
            id: BindingId(0),
            name: "value".into(),
            kind: BindingKind::Import,
            mutable: false,
        };
        let mut module = Module::new(
            "app:///live-import.js",
            FunctionId(0),
            vec![function(
                0,
                1,
                vec![imported.clone()],
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::LoadBinding {
                            dst: Register(0),
                            binding: imported.id,
                        },
                        Instruction::Return { value: Register(0) },
                    ],
                )],
            )],
        );
        module.imports.push(Import {
            specifier: "./dependency.js".into(),
            imported: "value".into(),
            local: imported.id,
        });
        let vm = Vm::new(module, Limits::default()).unwrap();
        let cell = binding_cell(Value::Number(1.0));
        let bindings = HashMap::from([(imported.id, cell.clone())]);
        assert_eq!(
            vm.run_with_cells(bindings.clone()).unwrap(),
            Value::Number(1.0)
        );
        *cell.borrow_mut() = Value::Number(2.0);
        assert_eq!(vm.run_with_cells(bindings).unwrap(), Value::Number(2.0));
    }

    #[test]
    fn linked_module_binding_can_read_an_external_aot_live_cell() {
        let imported = Binding {
            id: BindingId(0),
            name: "value".into(),
            kind: BindingKind::Import,
            mutable: false,
        };
        let module = Module::new(
            "app:///mixed-import.js",
            FunctionId(0),
            vec![function(
                0,
                1,
                vec![imported.clone()],
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::LoadBinding {
                            dst: Register(0),
                            binding: imported.id,
                        },
                        Instruction::Return { value: Register(0) },
                    ],
                )],
            )],
        );
        let value = Rc::new(RefCell::new(Value::Number(1.0)));
        let getter_value = Rc::clone(&value);
        let setter_value = Rc::clone(&value);
        let cell = external_binding_cell(
            Value::function(move |_, _| getter_value.borrow().clone()),
            Value::function(move |_, arguments| {
                let next = arguments.first().cloned().unwrap_or(Value::Undefined);
                *setter_value.borrow_mut() = next.clone();
                next
            }),
        );
        let vm = Vm::new(module, Limits::default()).unwrap();
        let bindings = HashMap::from([(imported.id, cell)]);
        assert_eq!(
            vm.run_with_cells(bindings.clone()).unwrap(),
            Value::Number(1.0)
        );
        *value.borrow_mut() = Value::Number(2.0);
        assert_eq!(vm.run_with_cells(bindings).unwrap(), Value::Number(2.0));
    }

    #[test]
    fn async_frame_resumes_from_the_shared_promise_microtask_queue() {
        let awaited = Binding {
            id: BindingId(0),
            name: "awaited".into(),
            kind: BindingKind::Import,
            mutable: false,
        };
        let mut module = Module::new(
            "app:///await.js",
            FunctionId(0),
            vec![Function {
                id: FunctionId(0),
                name: Some("asyncEntry".into()),
                parameters: Vec::new(),
                rest_parameter: None,
                arguments_binding: None,
                bindings: vec![awaited.clone()],
                captures: Vec::new(),
                this_binding: None,
                registers: 2,
                entry: BlockId(0),
                blocks: vec![
                    block(
                        0,
                        vec![
                            Instruction::LoadBinding {
                                dst: Register(0),
                                binding: awaited.id,
                            },
                            Instruction::Await {
                                dst: Register(1),
                                value: Register(0),
                                suspension: w3cos_ir::SuspensionId(0),
                            },
                            Instruction::Jump { target: BlockId(1) },
                        ],
                    ),
                    block(1, vec![Instruction::Return { value: Register(1) }]),
                    block(2, vec![Instruction::Throw { value: Register(1) }]),
                ],
                exception_regions: Vec::new(),
                suspension_points: vec![w3cos_ir::SuspensionPoint {
                    id: w3cos_ir::SuspensionId(0),
                    await_block: BlockId(0),
                    resume_block: BlockId(1),
                    reject_block: BlockId(2),
                    live_registers: vec![Register(0)],
                }],
                generator_suspension_points: Vec::new(),
                is_async: true,
                is_generator: false,
                source_span: None,
            }],
        );
        module.imports.push(Import {
            specifier: "w3cos:global".into(),
            imported: "awaited".into(),
            local: awaited.id,
        });

        let vm = Vm::new(module.clone(), Limits::default()).unwrap();
        let promise = vm
            .run_with_bindings(HashMap::from([(
                awaited.id,
                w3cos_core::promise::resolve(vec![Value::string("resumed")]),
            )]))
            .unwrap();
        {
            let budget = vm.0.execution_budget.borrow();
            assert_eq!(budget.nesting, 0);
            assert!(budget.active_since.is_none());
            assert!(
                budget
                    .remaining
                    .is_some_and(|remaining| !remaining.is_zero())
            );
        }
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        promise.call_method(
            "then",
            vec![Value::function(move |_, arguments| {
                *callback_observed.borrow_mut() =
                    arguments.first().cloned().unwrap_or(Value::Undefined);
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(*observed.borrow(), Value::string("resumed"));

        let deadline_promise = Vm::new(
            module,
            Limits {
                max_wall_time: Some(Duration::ZERO),
                ..Limits::default()
            },
        )
        .unwrap()
        .run_with_bindings(HashMap::from([(
            awaited.id,
            w3cos_core::promise::resolve(vec![Value::string("unreachable")]),
        )]))
        .unwrap();
        assert!(matches!(
            w3cos_core::promise::status(&deadline_promise),
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason))
                if reason.to_js_string().contains("WallClockLimitExceeded")
        ));
    }

    #[test]
    fn aot_and_vm_fixtures_share_host_property_call_and_add_semantics() {
        let host_binding = Binding {
            id: BindingId(0),
            name: "makeRecord".into(),
            kind: BindingKind::Import,
            mutable: false,
        };
        let mut module = Module::new(
            "app:///differential.js",
            FunctionId(0),
            vec![function(
                0,
                8,
                vec![host_binding.clone()],
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::LoadBinding {
                            dst: Register(0),
                            binding: host_binding.id,
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
                        Instruction::LoadConstant {
                            dst: Register(3),
                            value: Constant::String("count".into()),
                        },
                        Instruction::LoadConstant {
                            dst: Register(4),
                            value: Constant::Number(2.0),
                        },
                        Instruction::SetProperty {
                            object: Register(2),
                            key: Register(3),
                            value: Register(4),
                        },
                        Instruction::GetProperty {
                            dst: Register(5),
                            object: Register(2),
                            key: Register(3),
                        },
                        Instruction::LoadConstant {
                            dst: Register(6),
                            value: Constant::Number(3.0),
                        },
                        Instruction::Add {
                            dst: Register(7),
                            lhs: Register(5),
                            rhs: Register(6),
                        },
                        Instruction::Return { value: Register(7) },
                    ],
                )],
            )],
        );
        module.imports.push(Import {
            specifier: "w3cos:host".into(),
            imported: "makeRecord".into(),
            local: host_binding.id,
        });

        let host = Value::function(|_, _| Value::object(HashMap::new()));
        let vm_result = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(host_binding.id, host.clone())]))
            .unwrap();

        let aot_object = w3cos_core::intrinsics::call(&host, Value::Undefined, Vec::new());
        let key = Value::string("count");
        w3cos_core::intrinsics::set_property(&aot_object, &key, Value::Number(2.0));
        let aot_result = w3cos_core::intrinsics::add(
            &w3cos_core::intrinsics::get_property(&aot_object, &key),
            &Value::Number(3.0),
        );
        assert_eq!(vm_result, aot_result);
    }

    #[test]
    fn aot_and_vm_aggregate_creation_use_the_same_intrinsics() {
        let module = Module::new(
            "app:///aggregate-differential.js",
            FunctionId(0),
            vec![function(
                0,
                7,
                Vec::new(),
                Vec::new(),
                vec![block(
                    0,
                    vec![
                        Instruction::LoadConstant {
                            dst: Register(0),
                            value: Constant::String("status".into()),
                        },
                        Instruction::LoadConstant {
                            dst: Register(1),
                            value: Constant::String("ready".into()),
                        },
                        Instruction::LoadConstant {
                            dst: Register(2),
                            value: Constant::String("tiles".into()),
                        },
                        Instruction::LoadConstant {
                            dst: Register(3),
                            value: Constant::Number(1.0),
                        },
                        Instruction::LoadConstant {
                            dst: Register(4),
                            value: Constant::Number(2.0),
                        },
                        Instruction::CreateArray {
                            dst: Register(5),
                            elements: vec![Register(3), Register(4)],
                        },
                        Instruction::CreateObject {
                            dst: Register(6),
                            properties: vec![
                                (Register(0), Register(1)),
                                (Register(2), Register(5)),
                            ],
                        },
                        Instruction::Return { value: Register(6) },
                    ],
                )],
            )],
        );
        let vm_result = Vm::new(module, Limits::default()).unwrap().run().unwrap();
        let aot_result = w3cos_core::intrinsics::create_object(vec![
            (Value::string("status"), Value::string("ready")),
            (
                Value::string("tiles"),
                w3cos_core::intrinsics::create_array(vec![Value::Number(1.0), Value::Number(2.0)]),
            ),
        ]);

        assert_eq!(
            vm_result.get_property("status"),
            aot_result.get_property("status")
        );
        assert_eq!(
            vm_result.get_property("tiles").get_property("1"),
            aot_result.get_property("tiles").get_property("1")
        );
    }

    #[test]
    fn host_exceptions_enter_vm_catch_regions() {
        let host_binding = Binding {
            id: BindingId(0),
            name: "fail".into(),
            kind: BindingKind::Import,
            mutable: false,
        };
        let mut entry = function(
            0,
            3,
            vec![host_binding.clone()],
            Vec::new(),
            vec![
                block(
                    0,
                    vec![
                        Instruction::LoadBinding {
                            dst: Register(0),
                            binding: host_binding.id,
                        },
                        Instruction::LoadConstant {
                            dst: Register(1),
                            value: Constant::Undefined,
                        },
                        Instruction::Call {
                            dst: Register(1),
                            callee: Register(0),
                            this_value: Register(1),
                            arguments: Vec::new(),
                        },
                        Instruction::Return { value: Register(1) },
                    ],
                ),
                block(1, vec![Instruction::Return { value: Register(2) }]),
            ],
        );
        entry.exception_regions.push(ExceptionRegion {
            protected_blocks: vec![BlockId(0)],
            catch_block: Some(BlockId(1)),
            finally_block: None,
            exception: Register(2),
        });
        let mut module = Module::new("app:///catch.js", entry.id, vec![entry]);
        module.imports.push(Import {
            specifier: "w3cos:host".into(),
            imported: "fail".into(),
            local: host_binding.id,
        });
        let fail = Value::function(|_, _| w3cos_core::throw_value(Value::string("host failure")));
        assert_eq!(
            Vm::new(module, Limits::default())
                .unwrap()
                .run_with_bindings(HashMap::from([(host_binding.id, fail)]))
                .unwrap(),
            Value::string("host failure")
        );
    }

    #[test]
    fn native_code_calls_vm_functions_through_the_shared_callable_abi() {
        let parameter = Binding {
            id: BindingId(0),
            name: "input".into(),
            kind: BindingKind::Parameter,
            mutable: true,
        };
        let mut entry = function(
            0,
            3,
            vec![parameter.clone()],
            Vec::new(),
            vec![block(
                0,
                vec![
                    Instruction::LoadBinding {
                        dst: Register(0),
                        binding: parameter.id,
                    },
                    Instruction::LoadConstant {
                        dst: Register(1),
                        value: Constant::String(" from VM".into()),
                    },
                    Instruction::Add {
                        dst: Register(2),
                        lhs: Register(0),
                        rhs: Register(1),
                    },
                    Instruction::Return { value: Register(2) },
                ],
            )],
        );
        entry.parameters.push(parameter.id);
        let vm = Vm::new(
            Module::new("app:///callable.js", entry.id, vec![entry]),
            Limits::default(),
        )
        .unwrap();
        let callable = vm.callable(FunctionId(0), HashMap::new()).unwrap();
        assert_eq!(
            w3cos_core::intrinsics::call(&callable, Value::Undefined, vec![Value::string("hello")],),
            Value::string("hello from VM")
        );
    }

    #[test]
    fn call_frame_collects_remaining_arguments_into_the_w3ir_rest_parameter() {
        let first = Binding {
            id: BindingId(0),
            name: "first".into(),
            kind: BindingKind::Parameter,
            mutable: true,
        };
        let rest = Binding {
            id: BindingId(1),
            name: "rest".into(),
            kind: BindingKind::Parameter,
            mutable: true,
        };
        let mut entry = function(
            0,
            1,
            vec![first.clone(), rest.clone()],
            Vec::new(),
            vec![block(
                0,
                vec![
                    Instruction::LoadBinding {
                        dst: Register(0),
                        binding: rest.id,
                    },
                    Instruction::Return { value: Register(0) },
                ],
            )],
        );
        entry.parameters.push(first.id);
        entry.rest_parameter = Some(rest.id);
        let callable = Vm::new(
            Module::new("app:///rest.js", entry.id, vec![entry]),
            Limits::default(),
        )
        .unwrap()
        .callable(FunctionId(0), HashMap::new())
        .unwrap();

        let collected = w3cos_core::intrinsics::call(
            &callable,
            Value::Undefined,
            vec![
                Value::string("first"),
                Value::string("second"),
                Value::string("third"),
            ],
        );
        assert_eq!(collected.get_property("length"), Value::Number(2.0));
        assert_eq!(collected.get_property("0"), Value::string("second"));
        assert_eq!(collected.get_property("1"), Value::string("third"));
    }

    #[test]
    fn generator_frame_suspends_and_accepts_next_completion_values() {
        let entry = Function {
            id: FunctionId(0),
            name: Some("generatorEntry".into()),
            parameters: Vec::new(),
            rest_parameter: None,
            arguments_binding: None,
            bindings: Vec::new(),
            captures: Vec::new(),
            this_binding: None,
            registers: 2,
            entry: BlockId(0),
            blocks: vec![
                block(
                    0,
                    vec![
                        Instruction::LoadConstant {
                            dst: Register(0),
                            value: Constant::String("first".into()),
                        },
                        Instruction::Yield {
                            dst: Register(1),
                            value: Register(0),
                            suspension: w3cos_ir::SuspensionId(0),
                        },
                        Instruction::Jump { target: BlockId(1) },
                    ],
                ),
                block(1, vec![Instruction::Return { value: Register(1) }]),
                block(2, vec![Instruction::Throw { value: Register(1) }]),
                block(3, vec![Instruction::Return { value: Register(1) }]),
            ],
            exception_regions: Vec::new(),
            suspension_points: Vec::new(),
            generator_suspension_points: vec![w3cos_ir::GeneratorSuspensionPoint {
                id: w3cos_ir::SuspensionId(0),
                yield_block: BlockId(0),
                result: Register(1),
                resume_block: BlockId(1),
                throw_block: BlockId(2),
                return_block: BlockId(3),
                live_registers: vec![Register(0)],
            }],
            is_async: false,
            is_generator: true,
            source_span: None,
        };
        let module = Module::new("app:///generator.w3ir", FunctionId(0), vec![entry]);
        let generator = Vm::new(module.clone(), Limits::default())
            .unwrap()
            .run()
            .unwrap();

        let first = generator.call_method("next", Vec::new());
        assert_eq!(first.get_property("value"), Value::string("first"));
        assert_eq!(first.get_property("done"), Value::Bool(false));
        let completed = generator.call_method("next", vec![Value::Number(7.0)]);
        assert_eq!(completed.get_property("value"), Value::Number(7.0));
        assert_eq!(completed.get_property("done"), Value::Bool(true));

        let deadline_generator = Vm::new(
            module,
            Limits {
                max_wall_time: Some(Duration::ZERO),
                ..Limits::default()
            },
        )
        .unwrap()
        .run()
        .unwrap();
        let error = catch_unwind(AssertUnwindSafe(|| {
            deadline_generator.call_method("next", Vec::new())
        }))
        .expect_err("generator execution must stop when the wall-clock budget is exhausted");
        let reason = error
            .downcast_ref::<PanicValue>()
            .expect("generator limit failures cross the callable ABI as a JS throw");
        assert!(reason.0.to_js_string().contains("WallClockLimitExceeded"));
    }

    #[test]
    fn async_generator_requests_resolve_in_order_on_the_shared_microtask_queue() {
        let entry = Function {
            id: FunctionId(0),
            name: Some("asyncGeneratorEntry".into()),
            parameters: Vec::new(),
            rest_parameter: None,
            arguments_binding: None,
            bindings: Vec::new(),
            captures: Vec::new(),
            this_binding: None,
            registers: 2,
            entry: BlockId(0),
            blocks: vec![
                block(
                    0,
                    vec![
                        Instruction::LoadConstant {
                            dst: Register(0),
                            value: Constant::String("first".into()),
                        },
                        Instruction::Yield {
                            dst: Register(1),
                            value: Register(0),
                            suspension: w3cos_ir::SuspensionId(0),
                        },
                        Instruction::Jump { target: BlockId(1) },
                    ],
                ),
                block(1, vec![Instruction::Return { value: Register(1) }]),
                block(2, vec![Instruction::Throw { value: Register(1) }]),
                block(3, vec![Instruction::Return { value: Register(1) }]),
            ],
            exception_regions: Vec::new(),
            suspension_points: Vec::new(),
            generator_suspension_points: vec![w3cos_ir::GeneratorSuspensionPoint {
                id: w3cos_ir::SuspensionId(0),
                yield_block: BlockId(0),
                result: Register(1),
                resume_block: BlockId(1),
                throw_block: BlockId(2),
                return_block: BlockId(3),
                live_registers: vec![Register(0)],
            }],
            is_async: true,
            is_generator: true,
            source_span: None,
        };
        let generator = Vm::new(
            Module::new("app:///async-generator.w3ir", FunctionId(0), vec![entry]),
            Limits::default(),
        )
        .unwrap()
        .run()
        .unwrap();

        let first = generator.call_method("next", Vec::new());
        let completed = generator.call_method("next", vec![Value::Number(7.0)]);
        assert!(matches!(
            w3cos_core::promise::status(&first),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));
        assert!(matches!(
            w3cos_core::promise::status(&completed),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));
        w3cos_core::promise::drain_microtasks();

        let Some(w3cos_core::promise::PromiseStatus::Fulfilled(first)) =
            w3cos_core::promise::status(&first)
        else {
            panic!("first async-generator request did not fulfill");
        };
        assert_eq!(first.get_property("value"), Value::string("first"));
        assert_eq!(first.get_property("done"), Value::Bool(false));
        let Some(w3cos_core::promise::PromiseStatus::Fulfilled(completed)) =
            w3cos_core::promise::status(&completed)
        else {
            panic!("queued async-generator request did not fulfill");
        };
        assert_eq!(completed.get_property("value"), Value::Number(7.0));
        assert_eq!(completed.get_property("done"), Value::Bool(true));
    }
}
