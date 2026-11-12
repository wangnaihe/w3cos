//! Backend B (M1 item 13): W3IR → machine code through Cranelift.
//!
//! The rustc backend (`w3cos-compiler::w3ir_aot`) is the reference; this
//! backend must produce the same observable results (see
//! `examples/cranelift_diff.rs`, which cross-checks both on a test262 subset
//! and a micro/bench corpus).
//!
//! Slice 1 design:
//!
//! * Each sync W3IR function becomes one Cranelift function
//!   `extern "C" fn(*mut Frame) -> u32`. W3IR blocks map 1:1 to Cranelift
//!   blocks; `Jump`/`Branch`/`Return`/`Throw` are native control flow.
//! * Registers holding values the type pass proves numeric stay unboxed `f64`
//!   SSA values (Cranelift variables) and arithmetic / comparisons on them are
//!   native instructions. Everything else lives in the frame as boxed
//!   [`Value`]s and calls into the shared runtime (`w3cos_core::intrinsics`)
//!   through `extern "C"` helpers, so semantics come from the same core the
//!   rustc backend uses.
//! * Helpers never unwind into JIT frames: each runs under `catch_unwind` and
//!   returns a status (0 = normal, 1 = JS throw in `Frame::exception`,
//!   2 = Rust panic kept in `Frame::panic`). A non-zero status branches to the
//!   block's exception target (catch/finally, like the rustc backend) or
//!   returns it to the Rust entry, which rethrows.
//! * Bindings are `Rc<RefCell<(Value, initialized)>>` cells, the rustc
//!   backend's representation, so closures share captured cells through the
//!   same [`AotCaptureMap`].
//!
//! Anything outside the supported subset (async, generators, private names,
//! super, dynamic import, ...) makes [`compile_module`] return
//! [`Unsupported`]; callers fall back to the rustc backend / VM.

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

use cranelift_codegen::ir::{
    AbiParam, Block, InstBuilder, Signature, condcodes::FloatCC, condcodes::IntCC, types,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module as _};
use w3cos_core::{AotCapture, AotCaptureMap, Completion, Value, intrinsics};
use w3cos_ir::{
    BinaryOperator, BindingKind, BlockId, Constant, Function, FunctionId, Instruction, IrType,
    Module, UnaryOperator,
};

/// The module uses a W3IR construct slice 1 does not compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported by the Cranelift backend: {}", self.0)
    }
}

impl std::error::Error for Unsupported {}

type BindingCell = Rc<RefCell<(Value, bool)>>;

enum Slot {
    Local(BindingCell),
    Capture(AotCapture),
    Missing(u32),
}

/// Activation record shared between JIT code and the helpers.
pub struct Frame {
    regs: Vec<Value>,
    slots: Vec<Slot>,
    ret: Value,
    exception: Value,
    panic: Option<Box<dyn Any + Send>>,
    function: usize,
    program: Rc<Program>,
}

type Entry = unsafe extern "C" fn(*mut Frame) -> u32;

struct CompiledFunction {
    name: String,
    parameters: Vec<u32>,
    rest_slot: Option<u32>,
    arguments_slot: Option<u32>,
    this_slot: Option<u32>,
    /// Binding id and whether a fresh local starts initialized, per slot.
    slot_bindings: Vec<(u32, Option<bool>)>,
    slot_of: HashMap<u32, u32>,
    registers: usize,
    constants: Vec<Value>,
    lists: Vec<Vec<u32>>,
    func_id: FuncId,
    entry: Option<Entry>,
}

/// A compiled module. Function values created from it keep it alive.
pub struct Program {
    functions: Vec<CompiledFunction>,
    index_of: HashMap<u32, usize>,
    entry: usize,
    _module: RefCell<Option<JITModule>>,
    /// Cranelift compile + finalize time.
    pub compile_time: std::time::Duration,
    /// W3IR instructions compiled, and how many used native f64 paths.
    pub stats: Stats,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub functions: usize,
    pub instructions: usize,
    pub native_numeric: usize,
    pub ops: std::collections::BTreeMap<&'static str, usize>,
}

impl Program {
    /// Runs the module's entry function with no captures.
    pub fn run_entry(self: &Rc<Self>) -> Completion {
        self.run_entry_with(&AotCaptureMap::new())
    }

    /// Run the module entry with host-provided bindings (for example
    /// `w3cos:global` imports), keyed by binding id like the rustc backend.
    pub fn run_entry_with(self: &Rc<Self>, captures: &AotCaptureMap) -> Completion {
        self.invoke(self.entry, Value::Undefined, Vec::new(), captures)
    }

    /// A callable JS value for `function` closed over `captures`.
    pub fn function_value(
        self: &Rc<Self>,
        function: FunctionId,
        captures: AotCaptureMap,
    ) -> Option<Value> {
        let index = *self.index_of.get(&function.0)?;
        Some(make_function_value(
            Rc::clone(self),
            index,
            Rc::new(captures),
        ))
    }

    fn invoke(
        self: &Rc<Self>,
        index: usize,
        this: Value,
        args: Vec<Value>,
        captures: &AotCaptureMap,
    ) -> Completion {
        let function = &self.functions[index];
        let slots = function
            .slot_bindings
            .iter()
            .map(|(binding, local)| match (captures.get(binding), local) {
                (Some(capture), _) => Slot::Capture(capture.clone()),
                (None, Some(initialized)) => {
                    Slot::Local(Rc::new(RefCell::new((Value::Undefined, *initialized))))
                }
                (None, None) => Slot::Missing(*binding),
            })
            .collect();
        let mut frame = Frame {
            regs: vec![Value::Undefined; function.registers],
            slots,
            ret: Value::Undefined,
            exception: Value::Undefined,
            panic: None,
            function: index,
            program: Rc::clone(self),
        };
        for (position, slot) in function.parameters.iter().enumerate() {
            let value = args.get(position).cloned().unwrap_or(Value::Undefined);
            initialize_slot(&frame.slots[*slot as usize], value);
        }
        if let Some(slot) = function.rest_slot {
            let rest = args
                .iter()
                .skip(function.parameters.len())
                .cloned()
                .collect();
            initialize_slot(&frame.slots[slot as usize], intrinsics::create_array(rest));
        }
        if let Some(slot) = function.arguments_slot {
            initialize_slot(
                &frame.slots[slot as usize],
                intrinsics::create_array(args.clone()),
            );
        }
        if let Some(slot) = function.this_slot {
            initialize_slot(&frame.slots[slot as usize], this);
        }
        let entry = function.entry.expect("finalized");
        // SAFETY: `entry` was produced for this function's frame layout.
        let status = unsafe { entry(&mut frame) };
        match status {
            0 => Ok(std::mem::take(&mut frame.ret)),
            1 => Err(std::mem::take(&mut frame.exception)),
            _ => std::panic::resume_unwind(frame.panic.take().expect("panic payload")),
        }
    }
}

fn initialize_slot(slot: &Slot, value: Value) {
    match slot {
        Slot::Local(cell) => *cell.borrow_mut() = (value, true),
        Slot::Capture(AotCapture::Cell(cell)) => *cell.borrow_mut() = (value, true),
        _ => {}
    }
}

fn make_function_value(program: Rc<Program>, index: usize, captures: Rc<AotCaptureMap>) -> Value {
    let name = program.functions[index].name.clone();
    let length = program.functions[index].parameters.len() as u32;
    w3cos_core::named_function(&name, length, move |this, args| {
        w3cos_core::unwrap_or_throw(program.invoke(index, this, args, &captures))
    })
}

// ── helpers ────────────────────────────────────────────────────────────

/// Runs a helper body with the status protocol described in the crate docs.
fn guarded(frame: *mut Frame, body: impl FnOnce(&mut Frame) -> Result<(), Value>) -> u32 {
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: JIT code passes the frame pointer it was entered with.
        w3cos_core::catch_js_result(|| body(unsafe { &mut *frame }))
    }));
    // SAFETY: as above; the body's borrow has ended.
    let frame = unsafe { &mut *frame };
    match outcome {
        Ok(Ok(())) => 0,
        Ok(Err(exception)) => {
            frame.exception = exception;
            1
        }
        Err(payload) => {
            frame.panic = Some(payload);
            2
        }
    }
}

fn list(frame: &Frame, index: u32) -> &[u32] {
    &frame.program.functions[frame.function].lists[index as usize]
}

fn values(frame: &Frame, index: u32) -> Vec<Value> {
    list(frame, index)
        .iter()
        .map(|r| frame.regs[*r as usize].clone())
        .collect()
}

fn reg(frame: &Frame, r: u32) -> &Value {
    &frame.regs[r as usize]
}

macro_rules! helper {
    ($name:ident, |$f:ident, $a:ident, $b:ident, $c:ident, $d:ident| $body:expr) => {
        extern "C" fn $name(frame: *mut Frame, $a: u32, $b: u32, $c: u32, $d: u32) -> u32 {
            let _ = (&$a, &$b, &$c, &$d);
            guarded(frame, |$f: &mut Frame| $body)
        }
    };
}

helper!(h_const, |f, dst, index, _c, _d| {
    f.regs[dst as usize] = f.program.functions[f.function].constants[index as usize].clone();
    Ok(())
});
helper!(h_move, |f, dst, src, _c, _d| {
    f.regs[dst as usize] = reg(f, src).clone();
    Ok(())
});
helper!(h_load_binding, |f, dst, slot, _c, _d| {
    let value = match &f.slots[slot as usize] {
        Slot::Local(cell) | Slot::Capture(AotCapture::Cell(cell)) => {
            let binding = cell.borrow();
            if !binding.1 {
                return Err(intrinsics::reference_error("binding is not initialized"));
            }
            binding.0.clone()
        }
        Slot::Capture(AotCapture::Accessors(getter, _)) => getter.call_slice(Value::Undefined, &[]),
        Slot::Capture(AotCapture::Number(cell)) => {
            cell.get().map_or(Value::Undefined, Value::Number)
        }
        Slot::Missing(binding) => {
            return Err(intrinsics::reference_error(&format!(
                "binding {binding} is not defined"
            )));
        }
    };
    f.regs[dst as usize] = value;
    Ok(())
});
helper!(h_store_binding, |f, slot, src, _c, _d| {
    let value = reg(f, src).clone();
    match &f.slots[slot as usize] {
        Slot::Local(cell) | Slot::Capture(AotCapture::Cell(cell)) => {
            let mut binding = cell.borrow_mut();
            if !binding.1 {
                return Err(intrinsics::reference_error("binding is not initialized"));
            }
            binding.0 = value;
        }
        Slot::Capture(AotCapture::Accessors(_, setter)) => {
            if !setter.is_callable() {
                return Err(intrinsics::type_error("captured binding is immutable"));
            }
            setter.call_slice(Value::Undefined, std::slice::from_ref(&value));
        }
        Slot::Capture(AotCapture::Number(cell)) => cell.set(Some(value.to_number())),
        Slot::Missing(binding) => {
            return Err(intrinsics::reference_error(&format!(
                "binding {binding} is not defined"
            )));
        }
    }
    Ok(())
});
helper!(h_init_binding, |f, slot, src, _c, _d| {
    let value = reg(f, src).clone();
    match &f.slots[slot as usize] {
        Slot::Local(cell) | Slot::Capture(AotCapture::Cell(cell)) => {
            *cell.borrow_mut() = (value, true)
        }
        Slot::Capture(AotCapture::Accessors(_, setter)) if setter.is_callable() => {
            setter.call_slice(Value::Undefined, std::slice::from_ref(&value));
        }
        Slot::Capture(AotCapture::Number(cell)) => cell.set(Some(value.to_number())),
        _ => {}
    }
    Ok(())
});
helper!(h_refresh_binding, |f, slot, _b, _c, _d| {
    if let Slot::Local(cell) = &f.slots[slot as usize] {
        let fresh = cell.borrow().clone();
        f.slots[slot as usize] = Slot::Local(Rc::new(RefCell::new(fresh)));
    }
    Ok(())
});
helper!(h_add, |f, dst, lhs, rhs, _d| {
    f.regs[dst as usize] = intrinsics::add(reg(f, lhs), reg(f, rhs));
    Ok(())
});
helper!(h_binary, |f, dst, op, lhs, rhs| {
    let (l, r) = (reg(f, lhs), reg(f, rhs));
    let value = match binary_from_code(op) {
        BinaryOperator::Subtract => intrinsics::subtract(l, r),
        BinaryOperator::Multiply => intrinsics::multiply(l, r),
        BinaryOperator::Divide => intrinsics::divide(l, r),
        BinaryOperator::Remainder => intrinsics::remainder(l, r),
        BinaryOperator::Exponentiate => intrinsics::exponentiate(l, r),
        BinaryOperator::AbstractEqual => intrinsics::abstract_equal(l, r),
        BinaryOperator::AbstractNotEqual => {
            intrinsics::logical_not(&intrinsics::abstract_equal(l, r))
        }
        BinaryOperator::StrictEqual => intrinsics::strict_equal(l, r),
        BinaryOperator::StrictNotEqual => intrinsics::logical_not(&intrinsics::strict_equal(l, r)),
        BinaryOperator::LessThan => intrinsics::less_than(l, r),
        BinaryOperator::LessThanOrEqual => intrinsics::less_than_or_equal(l, r),
        BinaryOperator::GreaterThan => intrinsics::greater_than(l, r),
        BinaryOperator::GreaterThanOrEqual => intrinsics::greater_than_or_equal(l, r),
        BinaryOperator::LeftShift => intrinsics::left_shift(l, r),
        BinaryOperator::SignedRightShift => intrinsics::signed_right_shift(l, r),
        BinaryOperator::UnsignedRightShift => intrinsics::unsigned_right_shift(l, r),
        BinaryOperator::BitwiseOr => intrinsics::bitwise_or(l, r),
        BinaryOperator::BitwiseXor => intrinsics::bitwise_xor(l, r),
        BinaryOperator::BitwiseAnd => intrinsics::bitwise_and(l, r),
        BinaryOperator::InstanceOf => intrinsics::instance_of(l, r),
        BinaryOperator::In => intrinsics::in_operator(l, r),
    };
    f.regs[dst as usize] = value;
    Ok(())
});
helper!(h_unary, |f, dst, op, src, _d| {
    let v = reg(f, src);
    f.regs[dst as usize] = match op {
        0 => intrinsics::type_of(v),
        1 => intrinsics::negate(v),
        _ => intrinsics::bitwise_not(v),
    };
    Ok(())
});
helper!(h_get_property, |f, dst, object, key, _d| {
    f.regs[dst as usize] = intrinsics::get_property_checked(reg(f, object), reg(f, key));
    Ok(())
});
helper!(h_delete_property, |f, dst, object, key, _d| {
    f.regs[dst as usize] = intrinsics::delete_property(reg(f, object), reg(f, key));
    Ok(())
});
helper!(h_set_property, |f, object, key, value, strict| {
    let value = reg(f, value).clone();
    if strict != 0 {
        intrinsics::set_property_strict(reg(f, object), reg(f, key), value);
    } else {
        intrinsics::set_property(reg(f, object), reg(f, key), value);
    }
    Ok(())
});
helper!(h_define_field, |f, object, key, value, _d| {
    intrinsics::define_field(reg(f, object), reg(f, key), reg(f, value).clone());
    Ok(())
});
helper!(h_define_method, |f, object, key, value, _d| {
    intrinsics::define_method(reg(f, object), reg(f, key), reg(f, value).clone());
    Ok(())
});
helper!(h_create_object, |f, dst, list_index, _c, _d| {
    let pairs = list(f, list_index)
        .chunks(2)
        .map(|pair| (reg(f, pair[0]).clone(), reg(f, pair[1]).clone()))
        .collect();
    f.regs[dst as usize] = intrinsics::create_object(pairs);
    Ok(())
});
helper!(h_copy_data_properties, |f, target, source, _c, _d| {
    intrinsics::copy_data_properties(reg(f, target), reg(f, source));
    Ok(())
});
helper!(h_create_array, |f, dst, list_index, _c, _d| {
    f.regs[dst as usize] = intrinsics::create_array(values(f, list_index));
    Ok(())
});
helper!(h_append_element, |f, array, value, _c, _d| {
    intrinsics::append_array_element(reg(f, array), reg(f, value).clone());
    Ok(())
});
helper!(h_append_iterable, |f, array, iterable, _c, _d| {
    intrinsics::append_iterable(reg(f, array), reg(f, iterable));
    Ok(())
});
helper!(h_array_rest, |f, dst, value, start, _d| {
    f.regs[dst as usize] = intrinsics::array_rest(reg(f, value), start as usize);
    Ok(())
});
helper!(h_object_rest, |f, dst, value, list_index, _d| {
    let excluded = values(f, list_index);
    f.regs[dst as usize] = intrinsics::object_rest(reg(f, value), &excluded);
    Ok(())
});
helper!(h_create_closure, |f,
                           dst,
                           function_index,
                           list_index,
                           _d| {
    let mut captures = AotCaptureMap::new();
    let function = &f.program.functions[f.function];
    for binding in list(f, list_index) {
        let Some(slot) = function.slot_of.get(binding) else {
            continue;
        };
        match &f.slots[*slot as usize] {
            Slot::Local(cell) => {
                captures.insert(*binding, AotCapture::Cell(Rc::clone(cell)));
            }
            Slot::Capture(capture) => {
                captures.insert(*binding, capture.clone());
            }
            Slot::Missing(_) => {}
        }
    }
    let value = make_function_value(
        Rc::clone(&f.program),
        function_index as usize,
        Rc::new(captures),
    );
    f.regs[dst as usize] = value;
    Ok(())
});
helper!(h_create_class, |f,
                         dst,
                         constructor,
                         super_class,
                         initializer| {
    let constructor = reg(f, constructor);
    let super_class = (super_class != u32::MAX).then(|| reg(f, super_class));
    f.regs[dst as usize] = if initializer != u32::MAX {
        intrinsics::create_class_with_initializer(constructor, super_class, reg(f, initializer))
    } else {
        intrinsics::create_class(constructor, super_class)
    };
    Ok(())
});
helper!(h_call, |f, dst, callee, this_value, list_index| {
    let args = values(f, list_index);
    f.regs[dst as usize] =
        intrinsics::call_completion(reg(f, callee), reg(f, this_value).clone(), args)?;
    Ok(())
});
helper!(
    h_call_with_arguments,
    |f, dst, callee, this_value, arguments| {
        f.regs[dst as usize] = intrinsics::call_with_arguments_completion(
            reg(f, callee),
            reg(f, this_value).clone(),
            reg(f, arguments),
        )?;
        Ok(())
    }
);
helper!(h_call_method, |f, dst, object, key, list_index| {
    let args = values(f, list_index);
    f.regs[dst as usize] = intrinsics::call_method_completion(reg(f, object), reg(f, key), args)?;
    Ok(())
});
helper!(
    h_call_method_with_arguments,
    |f, dst, object, key, arguments| {
        f.regs[dst as usize] = intrinsics::call_method_with_arguments_completion(
            reg(f, object),
            reg(f, key),
            reg(f, arguments),
        )?;
        Ok(())
    }
);
helper!(h_construct, |f, dst, constructor, list_index, _d| {
    let args = values(f, list_index);
    f.regs[dst as usize] = intrinsics::construct_completion(reg(f, constructor), args)?;
    Ok(())
});
helper!(
    h_construct_with_arguments,
    |f, dst, constructor, arguments, _d| {
        f.regs[dst as usize] = intrinsics::construct_with_arguments_completion(
            reg(f, constructor),
            reg(f, arguments),
        )?;
        Ok(())
    }
);
helper!(h_return, |f, src, _b, _c, _d| {
    f.ret = reg(f, src).clone();
    Ok(())
});
helper!(h_throw, |f, src, _b, _c, _d| { Err(reg(f, src).clone()) });
helper!(h_catch, |f, dst, _b, _c, _d| {
    f.regs[dst as usize] = std::mem::take(&mut f.exception);
    Ok(())
});

/// `ToBoolean(reg)`: 0 / 1 (cannot throw).
extern "C" fn h_truthy(frame: *mut Frame, src: u32, _b: u32, _c: u32, _d: u32) -> u32 {
    // SAFETY: JIT code passes its own frame.
    let frame = unsafe { &*frame };
    u32::from(frame.regs[src as usize].to_bool())
}

/// Reads a register as a number (`NaN` for non-numbers); used only on
/// registers the type pass proved numeric.
extern "C" fn h_get_f64(frame: *mut Frame, src: u32) -> f64 {
    // SAFETY: JIT code passes its own frame.
    unsafe { &*frame }.regs[src as usize].to_number()
}

/// Boxes an unboxed f64 into a register.
extern "C" fn h_set_f64(frame: *mut Frame, dst: u32, value: f64) {
    // SAFETY: JIT code passes its own frame.
    unsafe { &mut *frame }.regs[dst as usize] = Value::Number(value);
}

extern "C" fn h_fmod(lhs: f64, rhs: f64) -> f64 {
    lhs % rhs
}

extern "C" fn h_to_int32(value: f64) -> i32 {
    intrinsics::f64_to_int32(value)
}

const BINARY_OPERATORS: [BinaryOperator; 21] = [
    BinaryOperator::Subtract,
    BinaryOperator::Multiply,
    BinaryOperator::Divide,
    BinaryOperator::Remainder,
    BinaryOperator::Exponentiate,
    BinaryOperator::AbstractEqual,
    BinaryOperator::AbstractNotEqual,
    BinaryOperator::StrictEqual,
    BinaryOperator::StrictNotEqual,
    BinaryOperator::LessThan,
    BinaryOperator::LessThanOrEqual,
    BinaryOperator::GreaterThan,
    BinaryOperator::GreaterThanOrEqual,
    BinaryOperator::LeftShift,
    BinaryOperator::SignedRightShift,
    BinaryOperator::UnsignedRightShift,
    BinaryOperator::BitwiseOr,
    BinaryOperator::BitwiseXor,
    BinaryOperator::BitwiseAnd,
    BinaryOperator::InstanceOf,
    BinaryOperator::In,
];

fn binary_code(operator: BinaryOperator) -> u32 {
    BINARY_OPERATORS
        .iter()
        .position(|op| *op == operator)
        .expect("operator") as u32
}

fn binary_from_code(code: u32) -> BinaryOperator {
    BINARY_OPERATORS[code as usize]
}

// ── compilation ────────────────────────────────────────────────────────

type HelperFn = extern "C" fn(*mut Frame, u32, u32, u32, u32) -> u32;

/// Compiles every function of `module`. Fails as a whole if any function
/// uses an unsupported construct.
pub fn compile_module(module: &Module) -> Result<Rc<Program>, Unsupported> {
    let started = std::time::Instant::now();
    let mut flags = settings::builder();
    flags
        .set("opt_level", "speed")
        .map_err(|e| Unsupported(e.to_string()))?;
    flags
        .set("is_pic", "false")
        .map_err(|e| Unsupported(e.to_string()))?;
    let isa = cranelift_native::builder()
        .map_err(|e| Unsupported(e.to_string()))?
        .finish(settings::Flags::new(flags))
        .map_err(|e| Unsupported(e.to_string()))?;
    let mut jit = JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ));
    let pointer = jit.target_config().pointer_type();

    let index_of: HashMap<u32, usize> = module
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| (function.id.0, index))
        .collect();
    let mut entry_sig = jit.make_signature();
    entry_sig.params.push(AbiParam::new(pointer));
    entry_sig.returns.push(AbiParam::new(types::I32));

    let mut stats = Stats::default();
    let mut functions = Vec::new();
    for function in &module.functions {
        check_supported(function)?;
        let func_id = jit
            .declare_function(
                &format!("w3ir_fn_{}", function.id.0),
                Linkage::Local,
                &entry_sig,
            )
            .map_err(|e| Unsupported(e.to_string()))?;
        functions.push(layout(function, func_id));
    }
    let mut ctx = jit.make_context();
    let mut builder_ctx = FunctionBuilderContext::new();
    for (index, function) in module.functions.iter().enumerate() {
        ctx.func.signature = entry_sig.clone();
        let mut emitter = Emitter {
            function,
            compiled: &mut functions[index],
            index_of: &index_of,
            pointer,
            stats: &mut stats,
            types: w3cos_ir::types::infer(function, false),
            target: jit.target_config(),
        };
        emitter.emit(&mut ctx.func, &mut builder_ctx)?;
        jit.define_function(functions[index].func_id, &mut ctx)
            .map_err(|e| Unsupported(format!("cranelift: {e:?}")))?;
        jit.clear_context(&mut ctx);
    }
    jit.finalize_definitions()
        .map_err(|e| Unsupported(e.to_string()))?;
    for compiled in &mut functions {
        let code = jit.get_finalized_function(compiled.func_id);
        // SAFETY: every function was defined with `entry_sig`.
        compiled.entry = Some(unsafe { std::mem::transmute::<*const u8, Entry>(code) });
    }
    stats.functions = functions.len();
    Ok(Rc::new(Program {
        entry: *index_of
            .get(&module.entry.0)
            .ok_or_else(|| Unsupported("missing entry".into()))?,
        functions,
        index_of,
        _module: RefCell::new(Some(jit)),
        compile_time: started.elapsed(),
        stats,
    }))
}

/// Whether every function of `module` is inside the supported subset.
pub fn check_module(module: &Module) -> Result<(), Unsupported> {
    module.functions.iter().try_for_each(check_supported)
}

fn check_supported(function: &Function) -> Result<(), Unsupported> {
    if function.is_async || function.is_generator {
        return Err(Unsupported("async / generator function".into()));
    }
    for block in &function.blocks {
        for instruction in &block.instructions {
            if let Some(name) = unsupported_name(instruction) {
                return Err(Unsupported(name.into()));
            }
        }
    }
    Ok(())
}

fn unsupported_name(instruction: &Instruction) -> Option<&'static str> {
    Some(match instruction {
        Instruction::DefinePrivate { .. } => "DefinePrivate",
        Instruction::GetPrivate { .. } => "GetPrivate",
        Instruction::SetPrivate { .. } => "SetPrivate",
        Instruction::HasPrivate { .. } => "HasPrivate",
        Instruction::DefinePrivateMethod { .. } => "DefinePrivateMethod",
        Instruction::DefinePrivateAccessor { .. } => "DefinePrivateAccessor",
        Instruction::DynamicImport { .. } => "DynamicImport",
        Instruction::ImportMeta { .. } => "ImportMeta",
        Instruction::Await { .. } => "Await",
        Instruction::Yield { .. } => "Yield",
        Instruction::YieldDelegate { .. } => "YieldDelegate",
        _ => return None,
    })
}

/// Name of a W3IR opcode (for coverage statistics).
pub fn op_name(instruction: &Instruction) -> &'static str {
    if let Some(name) = unsupported_name(instruction) {
        return name;
    }
    match instruction {
        Instruction::LoadConstant { .. } => "LoadConstant",
        Instruction::Move { .. } => "Move",
        Instruction::LoadBinding { .. } => "LoadBinding",
        Instruction::InitializeBinding { .. } => "InitializeBinding",
        Instruction::StoreBinding { .. } => "StoreBinding",
        Instruction::RefreshBinding { .. } => "RefreshBinding",
        Instruction::Add { .. } => "Add",
        Instruction::Binary { .. } => "Binary",
        Instruction::Unary { .. } => "Unary",
        Instruction::GetProperty { .. } => "GetProperty",
        Instruction::DeleteProperty { .. } => "DeleteProperty",
        Instruction::SetProperty { .. } => "SetProperty",
        Instruction::DefineField { .. } => "DefineField",
        Instruction::DefineMethod { .. } => "DefineMethod",
        Instruction::CreateObject { .. } => "CreateObject",
        Instruction::CopyDataProperties { .. } => "CopyDataProperties",
        Instruction::CreateArray { .. } => "CreateArray",
        Instruction::AppendArrayElement { .. } => "AppendArrayElement",
        Instruction::AppendIterable { .. } => "AppendIterable",
        Instruction::ArrayRest { .. } => "ArrayRest",
        Instruction::ObjectRest { .. } => "ObjectRest",
        Instruction::CreateClosure { .. } => "CreateClosure",
        Instruction::CreateClass { .. } => "CreateClass",
        Instruction::Call { .. } => "Call",
        Instruction::CallWithArguments { .. } => "CallWithArguments",
        Instruction::CallMethod { .. } => "CallMethod",
        Instruction::CallMethodWithArguments { .. } => "CallMethodWithArguments",
        Instruction::Construct { .. } => "Construct",
        Instruction::ConstructWithArguments { .. } => "ConstructWithArguments",
        Instruction::Jump { .. } => "Jump",
        Instruction::Branch { .. } => "Branch",
        Instruction::Return { .. } => "Return",
        Instruction::Throw { .. } => "Throw",
        _ => "other",
    }
}

fn binding_ids(instruction: &Instruction) -> Vec<u32> {
    match instruction {
        Instruction::LoadBinding { binding, .. }
        | Instruction::InitializeBinding { binding, .. }
        | Instruction::StoreBinding { binding, .. }
        | Instruction::RefreshBinding { binding } => vec![binding.0],
        Instruction::CreateClosure { captures, .. } => captures.iter().map(|b| b.0).collect(),
        _ => Vec::new(),
    }
}

fn layout(function: &Function, func_id: FuncId) -> CompiledFunction {
    let mut slot_bindings: Vec<(u32, Option<bool>)> = Vec::new();
    let mut slot_of = HashMap::new();
    let declared: HashMap<u32, BindingKind> =
        function.bindings.iter().map(|b| (b.id.0, b.kind)).collect();
    let mut add = |binding: u32, slot_bindings: &mut Vec<(u32, Option<bool>)>| {
        *slot_of.entry(binding).or_insert_with(|| {
            let local = declared.get(&binding).map(|kind| {
                !matches!(
                    kind,
                    BindingKind::Let
                        | BindingKind::Const
                        | BindingKind::Class
                        | BindingKind::Import
                        | BindingKind::Catch
                )
            });
            slot_bindings.push((binding, local));
            (slot_bindings.len() - 1) as u32
        })
    };
    for binding in &function.bindings {
        add(binding.id.0, &mut slot_bindings);
    }
    for binding in &function.captures {
        add(binding.0, &mut slot_bindings);
    }
    let parameters = function
        .parameters
        .iter()
        .map(|b| add(b.0, &mut slot_bindings))
        .collect();
    let rest_slot = function
        .rest_parameter
        .map(|b| add(b.0, &mut slot_bindings));
    let arguments_slot = function
        .arguments_binding
        .map(|b| add(b.0, &mut slot_bindings));
    let this_slot = function.this_binding.map(|b| add(b.0, &mut slot_bindings));
    for block in &function.blocks {
        for instruction in &block.instructions {
            for binding in binding_ids(instruction) {
                add(binding, &mut slot_bindings);
            }
        }
    }
    CompiledFunction {
        name: function.name.clone().unwrap_or_default(),
        parameters,
        rest_slot,
        arguments_slot,
        this_slot,
        slot_bindings,
        slot_of,
        registers: function.registers as usize,
        constants: Vec::new(),
        lists: Vec::new(),
        func_id,
        entry: None,
    }
}

struct Emitter<'a> {
    function: &'a Function,
    compiled: &'a mut CompiledFunction,
    index_of: &'a HashMap<u32, usize>,
    pointer: types::Type,
    stats: &'a mut Stats,
    types: w3cos_ir::types::TypedFunction,
    target: cranelift_codegen::isa::TargetFrontendConfig,
}

impl Emitter<'_> {
    fn constant(&mut self, value: Value) -> u32 {
        self.compiled.constants.push(value);
        (self.compiled.constants.len() - 1) as u32
    }

    fn list(&mut self, registers: Vec<u32>) -> u32 {
        self.compiled.lists.push(registers);
        (self.compiled.lists.len() - 1) as u32
    }

    fn slot(&self, binding: u32) -> u32 {
        self.compiled.slot_of[&binding]
    }

    fn is_number(&self, register: u32) -> bool {
        matches!(
            self.types.registers.get(register as usize),
            Some(IrType::Number | IrType::Int32)
        )
    }

    fn emit(
        &mut self,
        func: &mut cranelift_codegen::ir::Function,
        builder_ctx: &mut FunctionBuilderContext,
    ) -> Result<(), Unsupported> {
        let mut b = FunctionBuilder::new(func, builder_ctx);
        let pointer = self.pointer;
        let mut helper_sig = Signature::new(b.func.signature.call_conv);
        helper_sig.params.push(AbiParam::new(pointer));
        for _ in 0..4 {
            helper_sig.params.push(AbiParam::new(types::I32));
        }
        helper_sig.returns.push(AbiParam::new(types::I32));
        let helper_sig = b.import_signature(helper_sig);
        let mut get_sig = Signature::new(b.func.signature.call_conv);
        get_sig.params.push(AbiParam::new(pointer));
        get_sig.params.push(AbiParam::new(types::I32));
        get_sig.returns.push(AbiParam::new(types::F64));
        let get_sig = b.import_signature(get_sig);
        let mut set_sig = Signature::new(b.func.signature.call_conv);
        set_sig.params.push(AbiParam::new(pointer));
        set_sig.params.push(AbiParam::new(types::I32));
        set_sig.params.push(AbiParam::new(types::F64));
        let set_sig = b.import_signature(set_sig);
        let mut f2_sig = Signature::new(b.func.signature.call_conv);
        f2_sig.params.push(AbiParam::new(types::F64));
        f2_sig.params.push(AbiParam::new(types::F64));
        f2_sig.returns.push(AbiParam::new(types::F64));
        let f2_sig = b.import_signature(f2_sig);
        let mut i32_sig = Signature::new(b.func.signature.call_conv);
        i32_sig.params.push(AbiParam::new(types::F64));
        i32_sig.returns.push(AbiParam::new(types::I32));
        let i32_sig = b.import_signature(i32_sig);

        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let blocks: HashMap<BlockId, Block> = self
            .function
            .blocks
            .iter()
            .map(|block| (block.id, b.create_block()))
            .collect();
        b.switch_to_block(entry);
        let frame = b.block_params(entry)[0];

        // Unboxed f64 shadow of every numeric register, kept in sync with
        // the frame at boundaries (helpers read/write the boxed copy).
        let numeric: HashSet<u32> = (0..self.function.registers)
            .filter(|r| self.is_number(*r))
            .collect();
        let mut vars: HashMap<u32, Variable> = HashMap::new();
        for r in &numeric {
            let var = b.declare_var(types::F64);
            let zero = b.ins().f64const(0.0);
            b.def_var(var, zero);
            vars.insert(*r, var);
        }
        b.ins().jump(blocks[&self.function.entry], &[]);

        for block in &self.function.blocks {
            b.switch_to_block(blocks[&block.id]);
            let target = self
                .function
                .exception_regions
                .iter()
                .find(|region| region.protected_blocks.contains(&block.id))
                .and_then(|region| {
                    region
                        .catch_block
                        .or(region.finally_block)
                        .map(|t| (region.exception.0, blocks[&t]))
                });
            // Exception path for this block.
            let fail = b.create_block();
            b.append_block_param(fail, types::I32);

            for instruction in &block.instructions {
                self.stats.instructions += 1;
                *self.stats.ops.entry(op_name(instruction)).or_default() += 1;

                // Native f64 fast paths for proven-numeric operands.
                if self.emit_numeric(&mut b, frame, &vars, instruction, f2_sig, i32_sig, set_sig)
                    == Some(true)
                {
                    self.stats.native_numeric += 1;
                    continue;
                }

                let call =
                    |b: &mut FunctionBuilder, f: HelperFn, a: u32, bb: u32, c: u32, d: u32| {
                        let callee = b.ins().iconst(pointer, f as usize as i64);
                        let args = [
                            frame,
                            b.ins().iconst(types::I32, a as i64),
                            b.ins().iconst(types::I32, bb as i64),
                            b.ins().iconst(types::I32, c as i64),
                            b.ins().iconst(types::I32, d as i64),
                        ];
                        let inst = b.ins().call_indirect(helper_sig, callee, &args);
                        b.inst_results(inst)[0]
                    };
                let checked = |b: &mut FunctionBuilder, status| {
                    let next = b.create_block();
                    b.ins().brif(status, fail, &[status.into()], next, &[]);
                    b.switch_to_block(next);
                };
                // Before a helper reads numeric registers, box their shadows.
                let reads = boxed_reads(instruction);
                for r in reads {
                    if let Some(var) = vars.get(&r) {
                        let value = b.use_var(*var);
                        let callee = b
                            .ins()
                            .iconst(pointer, h_set_f64 as *const () as usize as i64);
                        let dst = b.ins().iconst(types::I32, r as i64);
                        b.ins().call_indirect(set_sig, callee, &[frame, dst, value]);
                    }
                }
                match instruction {
                    Instruction::LoadConstant { dst, value } => {
                        let value = match value {
                            Constant::Undefined => Value::Undefined,
                            Constant::Null => Value::Null,
                            Constant::Bool(v) => Value::Bool(*v),
                            Constant::Number(n) => Value::Number(*n),
                            Constant::String(s) => Value::from(s.as_str()),
                        };
                        let index = self.constant(value);
                        let s = call(&mut b, h_const, dst.0, index, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::Move { dst, src } => {
                        let s = call(&mut b, h_move, dst.0, src.0, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::LoadBinding { dst, binding } => {
                        let s = call(&mut b, h_load_binding, dst.0, self.slot(binding.0), 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::StoreBinding { binding, value } => {
                        let s = call(&mut b, h_store_binding, self.slot(binding.0), value.0, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::InitializeBinding { binding, value } => {
                        let s = call(&mut b, h_init_binding, self.slot(binding.0), value.0, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::RefreshBinding { binding } => {
                        let s = call(&mut b, h_refresh_binding, self.slot(binding.0), 0, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::Add { dst, lhs, rhs } => {
                        let s = call(&mut b, h_add, dst.0, lhs.0, rhs.0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::Binary {
                        dst,
                        operator,
                        lhs,
                        rhs,
                    } => {
                        let s = call(
                            &mut b,
                            h_binary,
                            dst.0,
                            binary_code(*operator),
                            lhs.0,
                            rhs.0,
                        );
                        checked(&mut b, s);
                    }
                    Instruction::Unary {
                        dst,
                        operator,
                        value,
                    } => {
                        let op = match operator {
                            UnaryOperator::TypeOf => 0,
                            UnaryOperator::Negate => 1,
                            UnaryOperator::BitwiseNot => 2,
                        };
                        let s = call(&mut b, h_unary, dst.0, op, value.0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::GetProperty { dst, object, key } => {
                        let s = call(&mut b, h_get_property, dst.0, object.0, key.0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::DeleteProperty { dst, object, key } => {
                        let s = call(&mut b, h_delete_property, dst.0, object.0, key.0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::SetProperty {
                        object,
                        key,
                        value,
                        strict,
                    } => {
                        let s = call(
                            &mut b,
                            h_set_property,
                            object.0,
                            key.0,
                            value.0,
                            u32::from(*strict),
                        );
                        checked(&mut b, s);
                    }
                    Instruction::DefineField { object, key, value } => {
                        let s = call(&mut b, h_define_field, object.0, key.0, value.0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::DefineMethod { object, key, value } => {
                        let s = call(&mut b, h_define_method, object.0, key.0, value.0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::CreateObject { dst, properties } => {
                        let list =
                            self.list(properties.iter().flat_map(|(k, v)| [k.0, v.0]).collect());
                        let s = call(&mut b, h_create_object, dst.0, list, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::CopyDataProperties { target, source } => {
                        let s = call(&mut b, h_copy_data_properties, target.0, source.0, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::CreateArray { dst, elements } => {
                        let list = self.list(elements.iter().map(|r| r.0).collect());
                        let s = call(&mut b, h_create_array, dst.0, list, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::AppendArrayElement { array, value } => {
                        let s = call(&mut b, h_append_element, array.0, value.0, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::AppendIterable { array, iterable } => {
                        let s = call(&mut b, h_append_iterable, array.0, iterable.0, 0, 0);
                        checked(&mut b, s);
                    }
                    Instruction::ArrayRest { dst, value, start } => {
                        let s = call(&mut b, h_array_rest, dst.0, value.0, *start, 0);
                        checked(&mut b, s);
                    }
                    Instruction::ObjectRest {
                        dst,
                        value,
                        excluded,
                    } => {
                        let list = self.list(excluded.iter().map(|r| r.0).collect());
                        let s = call(&mut b, h_object_rest, dst.0, value.0, list, 0);
                        checked(&mut b, s);
                    }
                    Instruction::CreateClosure {
                        dst,
                        function,
                        captures,
                    } => {
                        let index = *self
                            .index_of
                            .get(&function.0)
                            .ok_or_else(|| Unsupported("closure over a missing function".into()))?;
                        let list = self.list(captures.iter().map(|b| b.0).collect());
                        let s = call(&mut b, h_create_closure, dst.0, index as u32, list, 0);
                        checked(&mut b, s);
                    }
                    Instruction::CreateClass {
                        dst,
                        constructor,
                        super_class,
                        initializer,
                    } => {
                        let s = call(
                            &mut b,
                            h_create_class,
                            dst.0,
                            constructor.0,
                            super_class.map_or(u32::MAX, |r| r.0),
                            initializer.map_or(u32::MAX, |r| r.0),
                        );
                        checked(&mut b, s);
                    }
                    Instruction::Call {
                        dst,
                        callee,
                        this_value,
                        arguments,
                    } => {
                        let list = self.list(arguments.iter().map(|r| r.0).collect());
                        let s = call(&mut b, h_call, dst.0, callee.0, this_value.0, list);
                        checked(&mut b, s);
                    }
                    Instruction::CallWithArguments {
                        dst,
                        callee,
                        this_value,
                        arguments,
                    } => {
                        let s = call(
                            &mut b,
                            h_call_with_arguments,
                            dst.0,
                            callee.0,
                            this_value.0,
                            arguments.0,
                        );
                        checked(&mut b, s);
                    }
                    Instruction::CallMethod {
                        dst,
                        object,
                        key,
                        arguments,
                    } => {
                        let list = self.list(arguments.iter().map(|r| r.0).collect());
                        let s = call(&mut b, h_call_method, dst.0, object.0, key.0, list);
                        checked(&mut b, s);
                    }
                    Instruction::CallMethodWithArguments {
                        dst,
                        object,
                        key,
                        arguments,
                    } => {
                        let s = call(
                            &mut b,
                            h_call_method_with_arguments,
                            dst.0,
                            object.0,
                            key.0,
                            arguments.0,
                        );
                        checked(&mut b, s);
                    }
                    Instruction::Construct {
                        dst,
                        constructor,
                        arguments,
                    } => {
                        let list = self.list(arguments.iter().map(|r| r.0).collect());
                        let s = call(&mut b, h_construct, dst.0, constructor.0, list, 0);
                        checked(&mut b, s);
                    }
                    Instruction::ConstructWithArguments {
                        dst,
                        constructor,
                        arguments,
                    } => {
                        let s = call(
                            &mut b,
                            h_construct_with_arguments,
                            dst.0,
                            constructor.0,
                            arguments.0,
                            0,
                        );
                        checked(&mut b, s);
                    }
                    Instruction::Jump { target } => {
                        b.ins().jump(blocks[target], &[]);
                    }
                    Instruction::Branch {
                        condition,
                        then_block,
                        else_block,
                    } => {
                        if let Some(var) = vars.get(&condition.0) {
                            // ToBoolean(number): not 0, -0 or NaN.
                            let value = b.use_var(*var);
                            let zero = b.ins().f64const(0.0);
                            let truthy = b.ins().fcmp(FloatCC::OrderedNotEqual, value, zero);
                            b.ins()
                                .brif(truthy, blocks[then_block], &[], blocks[else_block], &[]);
                        } else {
                            let s = call(&mut b, h_truthy, condition.0, 0, 0, 0);
                            b.ins()
                                .brif(s, blocks[then_block], &[], blocks[else_block], &[]);
                        }
                    }
                    Instruction::Return { value } => {
                        let s = call(&mut b, h_return, value.0, 0, 0, 0);
                        checked(&mut b, s);
                        let zero = b.ins().iconst(types::I32, 0);
                        b.ins().return_(&[zero]);
                    }
                    Instruction::Throw { value } => {
                        let s = call(&mut b, h_throw, value.0, 0, 0, 0);
                        b.ins().jump(fail, &[s.into()]);
                    }
                    other => return Err(Unsupported(op_name(other).into())),
                }
                // A helper wrote a numeric register: refresh its shadow.
                if let Some(dst) = written_register(instruction)
                    && let Some(var) = vars.get(&dst)
                {
                    let callee = b
                        .ins()
                        .iconst(pointer, h_get_f64 as *const () as usize as i64);
                    let index = b.ins().iconst(types::I32, dst as i64);
                    let inst = b.ins().call_indirect(get_sig, callee, &[frame, index]);
                    let value = b.inst_results(inst)[0];
                    b.def_var(*var, value);
                }
            }

            // Exception path: status 1 goes to the catch/finally target,
            // anything else (or no target) returns to the Rust entry.
            b.switch_to_block(fail);
            let status = b.block_params(fail)[0];
            match target {
                Some((exception, target_block)) => {
                    let is_throw = b.ins().icmp_imm_s(IntCC::Equal, status, 1);
                    let catch = b.create_block();
                    let bail = b.create_block();
                    b.ins().brif(is_throw, catch, &[], bail, &[]);
                    b.switch_to_block(catch);
                    let callee = b
                        .ins()
                        .iconst(pointer, h_catch as *const () as usize as i64);
                    let args = [
                        frame,
                        b.ins().iconst(types::I32, exception as i64),
                        b.ins().iconst(types::I32, 0),
                        b.ins().iconst(types::I32, 0),
                        b.ins().iconst(types::I32, 0),
                    ];
                    b.ins().call_indirect(helper_sig, callee, &args);
                    if let Some(var) = vars.get(&exception) {
                        let callee = b
                            .ins()
                            .iconst(pointer, h_get_f64 as *const () as usize as i64);
                        let index = b.ins().iconst(types::I32, exception as i64);
                        let inst = b.ins().call_indirect(get_sig, callee, &[frame, index]);
                        let value = b.inst_results(inst)[0];
                        b.def_var(*var, value);
                    }
                    b.ins().jump(target_block, &[]);
                    b.switch_to_block(bail);
                    b.ins().return_(&[status]);
                }
                None => {
                    b.ins().return_(&[status]);
                }
            }
        }
        b.seal_all_blocks();
        b.finalize(self.target);
        Ok(())
    }

    /// Native lowering of numeric instructions. `Some(true)` when the
    /// instruction was fully emitted.
    #[allow(clippy::too_many_arguments)]
    fn emit_numeric(
        &mut self,
        b: &mut FunctionBuilder,
        _frame: cranelift_codegen::ir::Value,
        vars: &HashMap<u32, Variable>,
        instruction: &Instruction,
        f2_sig: cranelift_codegen::ir::SigRef,
        i32_sig: cranelift_codegen::ir::SigRef,
        _set_sig: cranelift_codegen::ir::SigRef,
    ) -> Option<bool> {
        let pointer = self.pointer;
        let num = |r: &w3cos_ir::Register| vars.get(&r.0).copied();
        match instruction {
            Instruction::LoadConstant {
                dst,
                value: Constant::Number(n),
            } => {
                let var = num(dst)?;
                let value = b.ins().f64const(*n);
                b.def_var(var, value);
                Some(true)
            }
            Instruction::Move { dst, src } => {
                let (d, s) = (num(dst)?, num(src)?);
                let value = b.use_var(s);
                b.def_var(d, value);
                Some(true)
            }
            Instruction::Add { dst, lhs, rhs } => {
                let (d, l, r) = (num(dst)?, num(lhs)?, num(rhs)?);
                let (l, r) = (b.use_var(l), b.use_var(r));
                let value = b.ins().fadd(l, r);
                b.def_var(d, value);
                Some(true)
            }
            Instruction::Unary {
                dst,
                operator: UnaryOperator::Negate,
                value,
            } => {
                let (d, v) = (num(dst)?, num(value)?);
                let v = b.use_var(v);
                let value = b.ins().fneg(v);
                b.def_var(d, value);
                Some(true)
            }
            Instruction::Binary {
                dst,
                operator,
                lhs,
                rhs,
            } => {
                let (l, r) = (num(lhs)?, num(rhs)?);
                let (lv, rv) = (b.use_var(l), b.use_var(r));
                let compare = |b: &mut FunctionBuilder, cc| {
                    let flag = b.ins().fcmp(cc, lv, rv);
                    (flag, true)
                };
                let (value, is_bool) = match operator {
                    BinaryOperator::Subtract => (b.ins().fsub(lv, rv), false),
                    BinaryOperator::Multiply => (b.ins().fmul(lv, rv), false),
                    BinaryOperator::Divide => (b.ins().fdiv(lv, rv), false),
                    BinaryOperator::Remainder => {
                        let callee = b.ins().iconst(pointer, h_fmod as *const () as usize as i64);
                        let inst = b.ins().call_indirect(f2_sig, callee, &[lv, rv]);
                        (b.inst_results(inst)[0], false)
                    }
                    BinaryOperator::BitwiseOr
                    | BinaryOperator::BitwiseAnd
                    | BinaryOperator::BitwiseXor => {
                        let callee = b
                            .ins()
                            .iconst(pointer, h_to_int32 as *const () as usize as i64);
                        let li = b.ins().call_indirect(i32_sig, callee, &[lv]);
                        let li = b.inst_results(li)[0];
                        let ri = b.ins().call_indirect(i32_sig, callee, &[rv]);
                        let ri = b.inst_results(ri)[0];
                        let v = match operator {
                            BinaryOperator::BitwiseOr => b.ins().bor(li, ri),
                            BinaryOperator::BitwiseAnd => b.ins().band(li, ri),
                            _ => b.ins().bxor(li, ri),
                        };
                        (b.ins().fcvt_from_sint(types::F64, v), false)
                    }
                    BinaryOperator::LessThan => compare(b, FloatCC::LessThan),
                    BinaryOperator::LessThanOrEqual => compare(b, FloatCC::LessThanOrEqual),
                    BinaryOperator::GreaterThan => compare(b, FloatCC::GreaterThan),
                    BinaryOperator::GreaterThanOrEqual => compare(b, FloatCC::GreaterThanOrEqual),
                    BinaryOperator::StrictEqual | BinaryOperator::AbstractEqual => {
                        compare(b, FloatCC::Equal)
                    }
                    BinaryOperator::StrictNotEqual | BinaryOperator::AbstractNotEqual => {
                        compare(b, FloatCC::NotEqual)
                    }
                    _ => return None,
                };
                if is_bool {
                    // Booleans stay boxed: write `true`/`false` to the frame.
                    let _ = value;
                    return None;
                }
                let d = num(dst)?;
                b.def_var(d, value);
                Some(true)
            }
            _ => None,
        }
    }
}

/// Registers an instruction reads through a helper (boxed).
fn boxed_reads(instruction: &Instruction) -> Vec<u32> {
    let mut regs = Vec::new();
    let mut push = |r: &w3cos_ir::Register| regs.push(r.0);
    match instruction {
        Instruction::Move { src, .. } => push(src),
        Instruction::InitializeBinding { value, .. } | Instruction::StoreBinding { value, .. } => {
            push(value)
        }
        Instruction::Add { lhs, rhs, .. } | Instruction::Binary { lhs, rhs, .. } => {
            push(lhs);
            push(rhs);
        }
        Instruction::Unary { value, .. } => push(value),
        Instruction::GetProperty { object, key, .. }
        | Instruction::DeleteProperty { object, key, .. } => {
            push(object);
            push(key);
        }
        Instruction::SetProperty {
            object, key, value, ..
        }
        | Instruction::DefineField { object, key, value }
        | Instruction::DefineMethod { object, key, value } => {
            push(object);
            push(key);
            push(value);
        }
        Instruction::CreateObject { properties, .. } => {
            for (k, v) in properties {
                push(k);
                push(v);
            }
        }
        Instruction::CopyDataProperties { target, source } => {
            push(target);
            push(source);
        }
        Instruction::CreateArray { elements, .. } => elements.iter().for_each(&mut push),
        Instruction::AppendArrayElement { array, value } => {
            push(array);
            push(value);
        }
        Instruction::AppendIterable { array, iterable } => {
            push(array);
            push(iterable);
        }
        Instruction::ArrayRest { value, .. } => push(value),
        Instruction::ObjectRest {
            value, excluded, ..
        } => {
            push(value);
            excluded.iter().for_each(&mut push);
        }
        Instruction::CreateClass {
            constructor,
            super_class,
            initializer,
            ..
        } => {
            push(constructor);
            if let Some(r) = super_class {
                push(r);
            }
            if let Some(r) = initializer {
                push(r);
            }
        }
        Instruction::Call {
            callee,
            this_value,
            arguments,
            ..
        } => {
            push(callee);
            push(this_value);
            arguments.iter().for_each(&mut push);
        }
        Instruction::CallWithArguments {
            callee,
            this_value,
            arguments,
            ..
        } => {
            push(callee);
            push(this_value);
            push(arguments);
        }
        Instruction::CallMethod {
            object,
            key,
            arguments,
            ..
        } => {
            push(object);
            push(key);
            arguments.iter().for_each(&mut push);
        }
        Instruction::CallMethodWithArguments {
            object,
            key,
            arguments,
            ..
        } => {
            push(object);
            push(key);
            push(arguments);
        }
        Instruction::Construct {
            constructor,
            arguments,
            ..
        } => {
            push(constructor);
            arguments.iter().for_each(&mut push);
        }
        Instruction::ConstructWithArguments {
            constructor,
            arguments,
            ..
        } => {
            push(constructor);
            push(arguments);
        }
        Instruction::Return { value } | Instruction::Throw { value } => push(value),
        _ => {}
    }
    regs
}

/// Register a (helper-emitted) instruction writes.
fn written_register(instruction: &Instruction) -> Option<u32> {
    match instruction {
        Instruction::LoadConstant { dst, .. }
        | Instruction::Move { dst, .. }
        | Instruction::LoadBinding { dst, .. }
        | Instruction::Add { dst, .. }
        | Instruction::Binary { dst, .. }
        | Instruction::Unary { dst, .. }
        | Instruction::GetProperty { dst, .. }
        | Instruction::DeleteProperty { dst, .. }
        | Instruction::CreateObject { dst, .. }
        | Instruction::CreateArray { dst, .. }
        | Instruction::ArrayRest { dst, .. }
        | Instruction::ObjectRest { dst, .. }
        | Instruction::CreateClosure { dst, .. }
        | Instruction::CreateClass { dst, .. }
        | Instruction::Call { dst, .. }
        | Instruction::CallWithArguments { dst, .. }
        | Instruction::CallMethod { dst, .. }
        | Instruction::CallMethodWithArguments { dst, .. }
        | Instruction::Construct { dst, .. }
        | Instruction::ConstructWithArguments { dst, .. } => Some(dst.0),
        _ => None,
    }
}
