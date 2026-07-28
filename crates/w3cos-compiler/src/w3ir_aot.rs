//! Native Rust state-machine emission from validated W3IR.
//!
//! This backend is deliberately compile-time only: generated applications
//! depend on `w3cos-core`, not `w3cos-ir` or `w3cos-vm`. W3VM and this emitter
//! therefore consume the same suspension/control-flow records without placing
//! a second JavaScript semantic implementation in ordinary AOT artifacts.

use anyhow::{Result, anyhow, bail};
use std::collections::{HashMap, HashSet};
use w3cos_ir::{
    BinaryOperator, BindingKind, Constant, Function, FunctionId, Instruction, Module, UnaryOperator,
};

#[derive(Clone, Copy)]
enum EmissionMode {
    Generator,
    AsyncGenerator,
    Async,
    Sync,
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
        for instruction in &block.instructions {
            blocks.push_str("                    ");
            let emitted = emit_instruction(
                instruction,
                exception_target,
                function,
                factories,
                EmissionMode::Sync,
                module_specifier,
            )?;
            if let Some((exception, target)) = exception_target
                && !matches!(
                    instruction,
                    Instruction::Jump { .. }
                        | Instruction::Branch { .. }
                        | Instruction::Return { .. }
                        | Instruction::Throw { .. }
                )
            {
                blocks.push_str(&format!(
                    "let __w3cos_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{ {emitted} }})); if let Err(__w3cos_payload) = __w3cos_outcome {{ let __w3cos_exception = if let Some(value) = __w3cos_payload.downcast_ref::<w3cos_core::PanicValue>() {{ value.0.clone() }} else {{ std::panic::resume_unwind(__w3cos_payload); }}; self.registers[{}] = __w3cos_exception; self.block = {}; continue 'drive; }}",
                    exception.0, target.0,
                ));
            } else {
                blocks.push_str(&emitted);
            }
            blocks.push('\n');
        }
        blocks.push_str("                }\n");
    }

    let mut binding_initializers = String::new();
    for binding in &function.bindings {
        let initialized = matches!(
            binding.kind,
            BindingKind::Var | BindingKind::Function | BindingKind::Parameter
        );
        binding_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized}))));\n",
            binding.id.0
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true))));\n",
            binding.0
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true))));\n",
            binding.0,
            function.parameters.len()
        ));
    }
    if let Some(binding) = function.this_binding {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__this, true))));\n",
            binding.0
        ));
    }

    Ok(format!(
        r#"
struct {type_name} {{
    registers: Vec<w3cos_core::Value>,
    bindings: std::collections::HashMap<
        u32,
        std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>,
    >,
    capture_getters: std::collections::HashMap<u32, w3cos_core::Value>,
    capture_setters: std::collections::HashMap<u32, w3cos_core::Value>,
    block: u32,
}}

impl {type_name} {{
    fn run(&mut self) -> w3cos_core::Value {{
        'drive: loop {{
            match self.block {{
{blocks}                _ => return w3cos_core::throw_value(
                    w3cos_core::Value::string("invalid synchronous W3IR block")
                ),
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
    let mut bindings = std::collections::HashMap::new();
{binding_initializers}{parameter_initializers}    let mut capture_getters = std::collections::HashMap::new();
    let mut capture_setters = std::collections::HashMap::new();
    for (binding, (getter, setter)) in __captures {{
        capture_getters.insert(binding, getter);
        capture_setters.insert(binding, setter);
    }}
    {type_name} {{
        registers: vec![w3cos_core::Value::Undefined; {registers}],
        bindings,
        capture_getters,
        capture_setters,
        block: {entry},
    }}.run()
}}
"#,
        registers = function.registers,
        entry = function.entry.0,
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
        for instruction in &block.instructions {
            blocks.push_str("                    ");
            let emitted = emit_instruction(
                instruction,
                exception_target,
                function,
                factories,
                EmissionMode::Async,
                module_specifier,
            )?
            .replace("__W3COS_ASYNC_FRAME__", &type_name);
            if let Some((exception, target)) = exception_target
                && !matches!(
                    instruction,
                    Instruction::Await { .. }
                        | Instruction::Jump { .. }
                        | Instruction::Branch { .. }
                        | Instruction::Return { .. }
                        | Instruction::Throw { .. }
                )
            {
                blocks.push_str(&format!(
                    "let __w3cos_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{ {emitted} }})); if let Err(__w3cos_payload) = __w3cos_outcome {{ let __w3cos_exception = if let Some(value) = __w3cos_payload.downcast_ref::<w3cos_core::PanicValue>() {{ value.0.clone() }} else {{ std::panic::resume_unwind(__w3cos_payload); }}; self.registers[{}] = __w3cos_exception; self.block = {}; continue 'drive; }}",
                    exception.0, target.0,
                ));
            } else {
                blocks.push_str(&emitted);
            }
            blocks.push('\n');
        }
        blocks.push_str("                }\n");
    }

    let mut binding_initializers = String::new();
    for binding in &function.bindings {
        let initialized = matches!(
            binding.kind,
            BindingKind::Var | BindingKind::Function | BindingKind::Parameter
        );
        binding_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized}))));\n",
            binding.id.0
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true))));\n",
            binding.0
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true))));\n",
            binding.0,
            function.parameters.len()
        ));
    }
    if let Some(binding) = function.this_binding {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__this, true))));\n",
            binding.0
        ));
    }

    Ok(format!(
        r#"
struct {type_name} {{
    registers: Vec<w3cos_core::Value>,
    bindings: std::collections::HashMap<
        u32,
        std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>,
    >,
    capture_getters: std::collections::HashMap<u32, w3cos_core::Value>,
    capture_setters: std::collections::HashMap<u32, w3cos_core::Value>,
    block: u32,
}}

impl {type_name} {{
    fn completed(value: w3cos_core::Value) -> w3cos_core::Value {{
        w3cos_core::Value::object(std::collections::HashMap::from([
            ("__w3cos_async_function_complete".to_string(), w3cos_core::Value::Bool(true)),
            ("value".to_string(), value),
        ]))
    }}

    fn awaited(
        value: w3cos_core::Value,
        dst: u32,
        resume_block: u32,
        reject_block: u32,
    ) -> w3cos_core::Value {{
        w3cos_core::Value::object(std::collections::HashMap::from([
            ("__w3cos_async_function_await".to_string(), w3cos_core::Value::Bool(true)),
            ("value".to_string(), value),
            ("dst".to_string(), w3cos_core::Value::Number(dst as f64)),
            ("resume".to_string(), w3cos_core::Value::Number(resume_block as f64)),
            ("reject".to_string(), w3cos_core::Value::Number(reject_block as f64)),
        ]))
    }}

    fn run(&mut self) -> w3cos_core::Value {{
        'drive: loop {{
            match self.block {{
{blocks}                _ => return w3cos_core::throw_value(
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
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{
            frame.borrow_mut().run()
        }}));
        let outcome = match outcome {{
            Ok(outcome) => outcome,
            Err(payload) => {{
                let reason = if let Some(value) =
                    payload.downcast_ref::<w3cos_core::PanicValue>()
                {{
                    value.0.clone()
                }} else {{
                    std::panic::resume_unwind(payload);
                }};
                reject.call(w3cos_core::Value::Undefined, vec![reason]);
                return;
            }}
        }};
        if outcome.get_property("__w3cos_async_function_complete").to_bool() {{
            resolve.call(
                w3cos_core::Value::Undefined,
                vec![outcome.get_property("value")],
            );
            return;
        }}
        if !outcome.get_property("__w3cos_async_function_await").to_bool() {{
            reject.call(
                w3cos_core::Value::Undefined,
                vec![w3cos_core::Value::string("invalid async W3IR outcome")],
            );
            return;
        }}
        let dst = outcome.get_property("dst").to_number() as usize;
        let resume_block = outcome.get_property("resume").to_number() as u32;
        let reject_block = outcome.get_property("reject").to_number() as u32;
        let fulfilled_frame = std::rc::Rc::clone(&frame);
        let fulfilled_resolve = resolve.clone();
        let fulfilled_reject = reject.clone();
        let on_fulfilled = w3cos_core::Value::function(move |_, arguments| {{
            {{
                let mut frame = fulfilled_frame.borrow_mut();
                frame.registers[dst] =
                    arguments.first().cloned().unwrap_or(w3cos_core::Value::Undefined);
                frame.block = resume_block;
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
    let mut bindings = std::collections::HashMap::new();
{binding_initializers}{parameter_initializers}    let mut capture_getters = std::collections::HashMap::new();
    let mut capture_setters = std::collections::HashMap::new();
    for (binding, (getter, setter)) in __captures {{
        capture_getters.insert(binding, getter);
        capture_setters.insert(binding, setter);
    }}
    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        registers: vec![w3cos_core::Value::Undefined; {registers}],
        bindings,
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
        registers = function.registers,
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
        for instruction in &block.instructions {
            blocks.push_str("                    ");
            let emitted = emit_instruction(
                instruction,
                exception_target,
                function,
                factories,
                EmissionMode::Generator,
                module_specifier,
            )?
            .replace("__W3COS_STATE__", &format!("{type_name}State"));
            if let Some((exception, target)) = exception_target
                && !matches!(
                    instruction,
                    Instruction::Yield { .. }
                        | Instruction::YieldDelegate { .. }
                        | Instruction::Jump { .. }
                        | Instruction::Branch { .. }
                        | Instruction::Return { .. }
                        | Instruction::Throw { .. }
                )
            {
                blocks.push_str(&format!(
                    "let __w3cos_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{ {emitted} }})); if let Err(__w3cos_payload) = __w3cos_outcome {{ let __w3cos_exception = if let Some(value) = __w3cos_payload.downcast_ref::<w3cos_core::PanicValue>() {{ value.0.clone() }} else {{ std::panic::resume_unwind(__w3cos_payload); }}; self.registers[{}] = __w3cos_exception; self.block = {}; continue 'drive; }}",
                    exception.0,
                    target.0,
                ));
            } else {
                blocks.push_str(&emitted);
            }
            blocks.push('\n');
        }
        blocks.push_str("                }\n");
    }

    let mut suspension_arms = String::new();
    let mut delegate_suspension_arms = String::new();
    for point in &function.generator_suspension_points {
        suspension_arms.push_str(&format!(
            "                    {} => {{ self.registers[{}] = input; self.block = match kind {{ 0 => {}, 1 => {}, _ => {} }}; }}\n",
            point.id.0,
            point.result.0,
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
            result = point.result.0,
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
        binding_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized}))));\n",
            binding.id.0
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true))));\n",
            binding.0
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true))));\n",
            binding.0,
            function.parameters.len()
        ));
    }
    if let Some(binding) = function.this_binding {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__this, true))));\n",
            binding.0
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
    bindings: std::collections::HashMap<
        u32,
        std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>,
    >,
    capture_getters: std::collections::HashMap<u32, w3cos_core::Value>,
    capture_setters: std::collections::HashMap<u32, w3cos_core::Value>,
    delegate_iterator: Option<w3cos_core::Value>,
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
    let mut bindings = std::collections::HashMap::new();
{binding_initializers}{parameter_initializers}    let mut capture_getters = std::collections::HashMap::new();
    let mut capture_setters = std::collections::HashMap::new();
    for (binding, (getter, setter)) in __captures {{
        capture_getters.insert(binding, getter);
        capture_setters.insert(binding, setter);
    }}
    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        state: {type_name}State::Start,
        registers: vec![w3cos_core::Value::Undefined; {registers}],
        bindings,
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
        registers = function.registers,
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
        for instruction in &block.instructions {
            blocks.push_str("                    ");
            let emitted = emit_instruction(
                instruction,
                exception_target,
                function,
                factories,
                EmissionMode::AsyncGenerator,
                module_specifier,
            )?
            .replace("__W3COS_STATE__", &format!("{type_name}State"));
            if let Some((exception, target)) = exception_target
                && !matches!(
                    instruction,
                    Instruction::Await { .. }
                        | Instruction::Yield { .. }
                        | Instruction::YieldDelegate { .. }
                        | Instruction::Jump { .. }
                        | Instruction::Branch { .. }
                        | Instruction::Return { .. }
                        | Instruction::Throw { .. }
                )
            {
                blocks.push_str(&format!(
                    "let __w3cos_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{ {emitted} }})); if let Err(__w3cos_payload) = __w3cos_outcome {{ let __w3cos_exception = if let Some(value) = __w3cos_payload.downcast_ref::<w3cos_core::PanicValue>() {{ value.0.clone() }} else {{ std::panic::resume_unwind(__w3cos_payload); }}; self.registers[{}] = __w3cos_exception; self.block = {}; continue 'drive; }}",
                    exception.0,
                    target.0,
                ));
            } else {
                blocks.push_str(&emitted);
            }
            blocks.push('\n');
        }
        blocks.push_str("                }\n");
    }

    let mut suspension_arms = String::new();
    let mut rejected_yield_arms = String::new();
    let mut delegate_resume_arms = String::new();
    let mut delegate_result_arms = String::new();
    for point in &function.generator_suspension_points {
        suspension_arms.push_str(&format!(
            "                    {} => {{ self.registers[{}] = input; self.block = match kind {{ 0 => {}, 1 => {}, _ => {} }}; }}\n",
            point.id.0,
            point.result.0,
            point.resume_block.0,
            point.return_block.0,
            point.throw_block.0,
        ));
        rejected_yield_arms.push_str(&format!(
            "            {} => {{ self.registers[{}] = reason; self.block = {}; self.state = {type_name}State::Ready; }}\n",
            point.id.0, point.result.0, point.throw_block.0
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
            result = point.result.0,
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
            result = point.result.0,
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
        binding_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::Value::Undefined, {initialized}))));\n",
            binding.id.0
        ));
    }
    let mut parameter_initializers = String::new();
    for (index, binding) in function.parameters.iter().enumerate() {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined), true))));\n",
            binding.0
        ));
    }
    if let Some(binding) = function.rest_parameter {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((w3cos_core::intrinsics::create_array(__args.iter().skip({}).cloned().collect()), true))));\n",
            binding.0,
            function.parameters.len()
        ));
    }
    if let Some(binding) = function.this_binding {
        parameter_initializers.push_str(&format!(
            "        bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((__this, true))));\n",
            binding.0
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
    bindings: std::collections::HashMap<
        u32,
        std::rc::Rc<std::cell::RefCell<(w3cos_core::Value, bool)>>,
    >,
    capture_getters: std::collections::HashMap<u32, w3cos_core::Value>,
    capture_setters: std::collections::HashMap<u32, w3cos_core::Value>,
    delegate_iterator: Option<w3cos_core::Value>,
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
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{
            method.call(iterator.clone(), arguments)
        }})) {{
            Ok(value) => Ok(Some(value)),
            Err(payload) => {{
                if let Some(value) = payload.downcast_ref::<w3cos_core::PanicValue>() {{
                    Err(value.0.clone())
                }} else {{
                    std::panic::resume_unwind(payload);
                }}
            }}
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
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{
            frame
                .borrow_mut()
                .resume(0, w3cos_core::Value::Undefined)
        }}));
        match outcome {{
            Ok(outcome) => Self::handle(frame, outcome),
            Err(payload) => {{
                let reason = if let Some(value) =
                    payload.downcast_ref::<w3cos_core::PanicValue>()
                {{
                    value.0.clone()
                }} else {{
                    std::panic::resume_unwind(payload);
                }};
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
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{
            frame.borrow_mut().resume(request.0, request.1)
        }}));
        match outcome {{
            Ok(outcome) => Self::handle(frame, outcome),
            Err(payload) => {{
                let reason = if let Some(value) =
                    payload.downcast_ref::<w3cos_core::PanicValue>()
                {{
                    value.0.clone()
                }} else {{
                    std::panic::resume_unwind(payload);
                }};
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
    let mut bindings = std::collections::HashMap::new();
{binding_initializers}{parameter_initializers}    let mut capture_getters = std::collections::HashMap::new();
    let mut capture_setters = std::collections::HashMap::new();
    for (binding, (getter, setter)) in __captures {{
        capture_getters.insert(binding, getter);
        capture_setters.insert(binding, setter);
    }}
    let frame = std::rc::Rc::new(std::cell::RefCell::new({type_name} {{
        state: {type_name}State::Start,
        registers: vec![w3cos_core::Value::Undefined; {registers}],
        bindings,
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
        registers = function.registers,
        entry = function.entry.0,
    ))
}

fn emit_instruction(
    instruction: &Instruction,
    exception_target: Option<(w3cos_ir::Register, w3cos_ir::BlockId)>,
    function: &Function,
    factories: &HashMap<FunctionId, String>,
    mode: EmissionMode,
    module_specifier: Option<&str>,
) -> Result<String> {
    let register = |register: w3cos_ir::Register| register.0;
    Ok(match instruction {
        Instruction::LoadConstant { dst, value } => format!(
            "self.registers[{}] = {};",
            register(*dst),
            emit_constant(value)
        ),
        Instruction::Move { dst, src } => format!(
            "self.registers[{}] = self.registers[{}].clone();",
            register(*dst),
            register(*src)
        ),
        Instruction::LoadBinding { dst, binding } => format!(
            "self.registers[{}] = if let Some(getter) = self.capture_getters.get(&{}) {{ getter.call(w3cos_core::Value::Undefined, Vec::new()) }} else {{ match self.bindings.get(&{}) {{ Some(cell) => {{ let binding = cell.borrow(); if binding.1 {{ binding.0.clone() }} else {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")) }} }}, None => w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")) }} }};",
            register(*dst),
            binding.0,
            binding.0
        ),
        Instruction::InitializeBinding { binding, value } => format!(
            "if let Some(binding) = self.bindings.get(&{}) {{ *binding.borrow_mut() = (self.registers[{}].clone(), true); }} else {{ self.bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new((self.registers[{}].clone(), true)))); }}",
            binding.0,
            register(*value),
            binding.0,
            register(*value)
        ),
        Instruction::StoreBinding { binding, value } => format!(
            "if let Some(setter) = self.capture_setters.get(&{}) {{ if !setter.is_callable() {{ w3cos_core::throw_value(w3cos_core::intrinsics::type_error(\"captured binding is immutable\")); }} setter.call(w3cos_core::Value::Undefined, vec![self.registers[{}].clone()]); }} else if let Some(binding) = self.bindings.get(&{}) {{ let mut binding = binding.borrow_mut(); if !binding.1 {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")); }} binding.0 = self.registers[{}].clone(); }} else {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"missing binding\")); }}",
            binding.0,
            register(*value),
            binding.0,
            register(*value)
        ),
        Instruction::RefreshBinding { binding } => format!(
            "if let Some(binding) = self.bindings.get(&{}) {{ let refreshed = binding.borrow().clone(); self.bindings.insert({}, std::rc::Rc::new(std::cell::RefCell::new(refreshed))); }}",
            binding.0, binding.0
        ),
        Instruction::Add { dst, lhs, rhs } => binary_call(*dst, *lhs, *rhs, "add"),
        Instruction::Binary {
            dst,
            operator,
            lhs,
            rhs,
        } => match operator {
            BinaryOperator::AbstractNotEqual => format!(
                "self.registers[{}] = w3cos_core::intrinsics::logical_not(&w3cos_core::intrinsics::abstract_equal(&self.registers[{}], &self.registers[{}]));",
                register(*dst),
                register(*lhs),
                register(*rhs)
            ),
            BinaryOperator::StrictNotEqual => format!(
                "self.registers[{}] = w3cos_core::intrinsics::logical_not(&w3cos_core::intrinsics::strict_equal(&self.registers[{}], &self.registers[{}]));",
                register(*dst),
                register(*lhs),
                register(*rhs)
            ),
            BinaryOperator::InstanceOf => format!(
                "self.registers[{}] = w3cos_core::intrinsics::instance_of(&self.registers[{}], &self.registers[{}]);",
                register(*dst),
                register(*lhs),
                register(*rhs)
            ),
            BinaryOperator::In => format!(
                "self.registers[{}] = w3cos_core::intrinsics::in_operator(&self.registers[{}], &self.registers[{}]);",
                register(*dst),
                register(*lhs),
                register(*rhs)
            ),
            operator => binary_call(*dst, *lhs, *rhs, binary_intrinsic(*operator)),
        },
        Instruction::Unary {
            dst,
            operator,
            value,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::{}(&self.registers[{}]);",
            register(*dst),
            unary_intrinsic(*operator),
            register(*value)
        ),
        Instruction::GetProperty { dst, object, key } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::get_property(&self.registers[{}], &self.registers[{}]);",
            register(*dst),
            register(*object),
            register(*key)
        ),
        Instruction::DeleteProperty { dst, object, key } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::delete_property(&self.registers[{}], &self.registers[{}]);",
            register(*dst),
            register(*object),
            register(*key)
        ),
        Instruction::SetProperty { object, key, value } => format!(
            "w3cos_core::intrinsics::set_property(&self.registers[{}], &self.registers[{}], self.registers[{}].clone());",
            register(*object),
            register(*key),
            register(*value)
        ),
        Instruction::DefineField { object, key, value } => format!(
            "w3cos_core::intrinsics::define_field(&self.registers[{}], &self.registers[{}], self.registers[{}].clone());",
            register(*object),
            register(*key),
            register(*value)
        ),
        Instruction::DefinePrivate {
            object,
            brand,
            name,
            value,
        } => format!(
            "w3cos_core::intrinsics::define_private(&self.registers[{}], &self.registers[{}], &self.registers[{}], self.registers[{}].clone());",
            register(*object),
            register(*brand),
            register(*name),
            register(*value)
        ),
        Instruction::GetPrivate {
            dst,
            object,
            brand,
            name,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::get_private(&self.registers[{}], &self.registers[{}], &self.registers[{}]);",
            register(*dst),
            register(*object),
            register(*brand),
            register(*name)
        ),
        Instruction::SetPrivate {
            object,
            brand,
            name,
            value,
        } => format!(
            "w3cos_core::intrinsics::set_private(&self.registers[{}], &self.registers[{}], &self.registers[{}], self.registers[{}].clone());",
            register(*object),
            register(*brand),
            register(*name),
            register(*value)
        ),
        Instruction::HasPrivate { dst, object, brand } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::has_private(&self.registers[{}], &self.registers[{}]);",
            register(*dst),
            register(*object),
            register(*brand)
        ),
        Instruction::DefinePrivateMethod { brand, name, value } => format!(
            "w3cos_core::intrinsics::define_private_method(&self.registers[{}], &self.registers[{}], self.registers[{}].clone());",
            register(*brand),
            register(*name),
            register(*value)
        ),
        Instruction::DefinePrivateAccessor {
            brand,
            name,
            getter,
            setter,
        } => {
            let getter = getter
                .map(|register| format!("Some(self.registers[{}].clone())", register.0))
                .unwrap_or_else(|| "None".into());
            let setter = setter
                .map(|register| format!("Some(self.registers[{}].clone())", register.0))
                .unwrap_or_else(|| "None".into());
            format!(
                "w3cos_core::intrinsics::define_private_accessor(&self.registers[{}], &self.registers[{}], {getter}, {setter});",
                register(*brand),
                register(*name)
            )
        }
        Instruction::CreateArray { dst, elements } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::create_array(vec![{}]);",
            register(*dst),
            elements
                .iter()
                .map(|element| format!("self.registers[{}].clone()", register(*element)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::AppendArrayElement { array, value } => format!(
            "w3cos_core::intrinsics::append_array_element(&self.registers[{}], self.registers[{}].clone());",
            register(*array),
            register(*value)
        ),
        Instruction::AppendIterable { array, iterable } => format!(
            "w3cos_core::intrinsics::append_iterable(&self.registers[{}], &self.registers[{}]);",
            register(*array),
            register(*iterable)
        ),
        Instruction::ArrayRest { dst, value, start } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::array_rest(&self.registers[{}], {});",
            register(*dst),
            register(*value),
            *start as usize
        ),
        Instruction::ObjectRest {
            dst,
            value,
            excluded,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::object_rest(&self.registers[{}], &[{}]);",
            register(*dst),
            register(*value),
            excluded
                .iter()
                .map(|key| format!("self.registers[{}].clone()", register(*key)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::CreateObject { dst, properties } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::create_object(vec![{}]);",
            register(*dst),
            properties
                .iter()
                .map(|(key, value)| format!(
                    "(self.registers[{}].clone(), self.registers[{}].clone())",
                    register(*key),
                    register(*value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::CopyDataProperties { target, source } => format!(
            "w3cos_core::intrinsics::copy_data_properties(&self.registers[{}], &self.registers[{}]);",
            register(*target),
            register(*source)
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
            for capture in captures {
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
                adapters.push_str(&format!(
                    "let __w3cos_pair = if let Some(getter) = self.capture_getters.get(&{}).cloned() {{ (getter, self.capture_setters.get(&{}).cloned().unwrap_or(w3cos_core::Value::Undefined)) }} else if let Some(cell) = self.bindings.get(&{}).cloned() {{ let getter = {{ let cell = std::rc::Rc::clone(&cell); w3cos_core::Value::function(move |_, _| {{ let binding = cell.borrow(); if binding.1 {{ binding.0.clone() }} else {{ w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"binding is not initialized\")) }} }}) }}; let setter = {local_setter}; (getter, setter) }} else {{ return w3cos_core::throw_value(w3cos_core::intrinsics::reference_error(\"missing nested generator capture\")); }}; __w3cos_nested_captures.insert({}, __w3cos_pair);",
                    capture.0, capture.0, capture.0, capture.0
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
                .map(|register| format!("Some(self.registers[{}].clone())", register.0))
                .unwrap_or_else(|| "None".into());
            let initializer = initializer
                .map(|register| format!("self.registers[{}].clone()", register.0))
                .unwrap_or_else(|| "w3cos_core::Value::Undefined".into());
            format!(
                "let __w3cos_constructor = self.registers[{}].clone(); let __w3cos_super_class = {super_class}; let __w3cos_initializer = {initializer}; self.registers[{}] = w3cos_core::intrinsics::create_class_with_initializer(&__w3cos_constructor, __w3cos_super_class.as_ref(), &__w3cos_initializer);",
                register(*constructor),
                register(*dst),
            )
        }
        Instruction::Call {
            dst,
            callee,
            this_value,
            arguments,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::call(&self.registers[{}], self.registers[{}].clone(), vec![{}]);",
            register(*dst),
            register(*callee),
            register(*this_value),
            emit_arguments(arguments)
        ),
        Instruction::CallWithArguments {
            dst,
            callee,
            this_value,
            arguments,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::call_with_arguments(&self.registers[{}], self.registers[{}].clone(), &self.registers[{}]);",
            register(*dst),
            register(*callee),
            register(*this_value),
            register(*arguments)
        ),
        Instruction::CallMethod {
            dst,
            object,
            key,
            arguments,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::call_method(&self.registers[{}], &self.registers[{}], vec![{}]);",
            register(*dst),
            register(*object),
            register(*key),
            emit_arguments(arguments)
        ),
        Instruction::CallMethodWithArguments {
            dst,
            object,
            key,
            arguments,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::call_method_with_arguments(&self.registers[{}], &self.registers[{}], &self.registers[{}]);",
            register(*dst),
            register(*object),
            register(*key),
            register(*arguments)
        ),
        Instruction::Construct {
            dst,
            constructor,
            arguments,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::construct(&self.registers[{}], vec![{}]);",
            register(*dst),
            register(*constructor),
            emit_arguments(arguments)
        ),
        Instruction::ConstructWithArguments {
            dst,
            constructor,
            arguments,
        } => format!(
            "self.registers[{}] = w3cos_core::intrinsics::construct_with_arguments(&self.registers[{}], &self.registers[{}]);",
            register(*dst),
            register(*constructor),
            register(*arguments)
        ),
        Instruction::DynamicImport { dst, specifier } => {
            let referrer = module_specifier
                .ok_or_else(|| anyhow!("dynamic import AOT emission requires its W3IR module"))?;
            format!(
                "self.registers[{}] = w3cos_core::host_modules::dynamic_import(self.registers[{}].clone(), w3cos_core::Value::string({referrer:?}));",
                register(*dst),
                register(*specifier)
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
                "self.state = __W3COS_STATE__::Suspended({}); return Self::result(self.registers[{}].clone(), false);",
                suspension.0,
                register(*value)
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
            let awaited = format!(
                "return Self::awaited(self.registers[{}].clone(), {}, {}, {});",
                register(*value),
                register(*dst),
                point.resume_block.0,
                point.reject_block.0,
            );
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
                    "let iterator = self.registers[{}].clone(); self.delegate_iterator = Some(iterator.clone()); match Self::delegate_call(&iterator, \"next\", Vec::new()) {{ Ok(Some(awaited)) => return Self::delegated(awaited, {}, 0), Ok(None) => {{ self.registers[{}] = w3cos_core::Value::string(\"TypeError: delegated iterator has no next method\"); self.delegate_iterator = None; self.block = {}; continue 'drive; }}, Err(reason) => {{ self.registers[{}] = reason; self.delegate_iterator = None; self.block = {}; continue 'drive; }} }}",
                    register(*iterator),
                    suspension.0,
                    register(*dst),
                    point.throw_block.0,
                    register(*dst),
                    point.throw_block.0,
                )
            } else {
                format!(
                    "let iterator = self.registers[{}].clone(); let delegated = Self::delegate_step(&iterator, \"next\", Vec::new()).unwrap_or_else(|| w3cos_core::throw_value(w3cos_core::Value::string(\"TypeError: delegated iterator has no next method\"))); let value = delegated.get_property(\"value\"); if delegated.get_property(\"done\").to_bool() {{ self.registers[{}] = value; self.block = {}; continue 'drive; }} self.delegate_iterator = Some(iterator); self.state = __W3COS_STATE__::SuspendedDelegate({}); return Self::result(value, false);",
                    register(*iterator),
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
        } => format!(
            "self.block = if self.registers[{}].to_bool() {{ {} }} else {{ {} }}; continue 'drive;",
            register(*condition),
            then_block.0,
            else_block.0
        ),
        Instruction::Return { value } => match mode {
            EmissionMode::Generator | EmissionMode::AsyncGenerator => format!(
                "self.state = __W3COS_STATE__::Completed; return Self::result(self.registers[{}].clone(), true);",
                register(*value)
            ),
            EmissionMode::Sync => {
                format!("return self.registers[{}].clone();", register(*value))
            }
            EmissionMode::Async => format!(
                "return __W3COS_ASYNC_FRAME__::completed(self.registers[{}].clone());",
                register(*value)
            ),
        },
        Instruction::Throw { value } => {
            if let Some((exception, target)) = exception_target {
                format!(
                    "self.registers[{}] = self.registers[{}].clone(); self.block = {}; continue 'drive;",
                    register(exception),
                    register(*value),
                    target.0
                )
            } else {
                match mode {
                    EmissionMode::Generator | EmissionMode::AsyncGenerator => format!(
                        "self.state = __W3COS_STATE__::Completed; return w3cos_core::throw_value(self.registers[{}].clone());",
                        register(*value)
                    ),
                    EmissionMode::Sync => format!(
                        "return w3cos_core::throw_value(self.registers[{}].clone());",
                        register(*value)
                    ),
                    EmissionMode::Async => format!(
                        "return w3cos_core::throw_value(self.registers[{}].clone());",
                        register(*value)
                    ),
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

fn binary_call(
    dst: w3cos_ir::Register,
    lhs: w3cos_ir::Register,
    rhs: w3cos_ir::Register,
    intrinsic: &str,
) -> String {
    format!(
        "self.registers[{}] = w3cos_core::intrinsics::{intrinsic}(&self.registers[{}], &self.registers[{}]);",
        dst.0, lhs.0, rhs.0
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

fn emit_arguments(arguments: &[w3cos_ir::Register]) -> String {
    arguments
        .iter()
        .map(|argument| format!("self.registers[{}].clone()", argument.0))
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
        assert!(generated.contains("__w3cos_async_function_await"));
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
}
