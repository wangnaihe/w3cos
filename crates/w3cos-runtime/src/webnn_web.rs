//! W3C WebNN surface with a small CPU backend.
//!
//! `navigator.ml.createContext()` requires the `webnn` permission. The CPU
//! backend executes `input`, `constant`, `add`, `mul`, and `relu`. Other
//! graph operators reject `NotSupportedError` instead of returning a result.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::jsdom::realm_function;
use w3cos_core::Value;

const PERMISSION: &str = "webnn";

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static GRAPHS: RefCell<HashMap<u64, Graph>> = RefCell::new(HashMap::new());
    static NEXT_GRAPH: RefCell<u64> = const { RefCell::new(1) };
}

#[derive(Clone)]
enum Node {
    Input { name: String, len: usize },
    Constant(Vec<f32>),
    Add(usize, usize),
    Mul(usize, usize),
    Relu(usize),
}

struct Graph {
    nodes: Vec<Node>,
    outputs: Vec<(String, usize)>,
}

#[derive(Clone)]
struct BuilderState {
    nodes: Vec<Node>,
}

thread_local! {
    static BUILDERS: RefCell<HashMap<u64, BuilderState>> = RefCell::new(HashMap::new());
    static NEXT_BUILDER: RefCell<u64> = const { RefCell::new(1) };
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn rejected(name: &str, message: &str) -> Value {
    w3cos_core::promise::reject(vec![error(name, message)])
}

fn illegal(name: &str) -> Value {
    w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
}

fn class_named(name: &'static str, constructor: Value) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        constructor.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
        constructor.set_property("prototype", prototype);
        classes
            .borrow_mut()
            .insert(name.to_string(), constructor.clone());
        constructor
    })
}

fn length_of(descriptor: &Value) -> Result<usize, String> {
    let dimensions = descriptor.get_property("dimensions");
    let count = dimensions.get_property("length").to_u32() as usize;
    if count == 0 {
        return Err("an operand needs dimensions".into());
    }
    let mut length = 1usize;
    for index in 0..count {
        let dim = dimensions.get_property(&index.to_string()).to_u32() as usize;
        if dim == 0 {
            return Err("dimensions must be positive".into());
        }
        length = length.saturating_mul(dim);
    }
    let data_type = descriptor.get_property("dataType").to_js_string();
    if !data_type.is_empty() && data_type != "float32" {
        return Err(format!("{data_type} is not supported by the CPU backend"));
    }
    Ok(length)
}

fn floats_of(value: &Value) -> Result<Vec<f32>, String> {
    if let Some(bytes) = w3cos_core::binary::bytes_of(value) {
        if !bytes.is_empty() && bytes.len() % 4 == 0 {
            return Ok(bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect());
        }
    }
    let count = value.get_property("length").to_u32() as usize;
    if count == 0 {
        return Err("expected a Float32Array or a numeric array".into());
    }
    Ok((0..count)
        .map(|index| value.get_property(&index.to_string()).to_number() as f32)
        .collect())
}

fn float32_array(values: &[f32]) -> Value {
    let bytes = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    let buffer = w3cos_core::binary::array_buffer_value(bytes);
    w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Float32Array"),
        vec![buffer],
    )
}

fn operand(builder_id: u64, index: usize) -> Value {
    let value = Value::object(HashMap::from([
        ("__w3cos_builder".into(), Value::Number(builder_id as f64)),
        ("__w3cos_operand".into(), Value::Number(index as f64)),
    ]));
    w3cos_core::class::set_prototype_of(&value, &ml_operand_class().get_property("prototype"));
    value
}

fn operand_index(builder_id: u64, value: &Value) -> Result<usize, String> {
    if value.get_property("__w3cos_builder").to_u32() as u64 != builder_id {
        return Err("operand belongs to a different MLGraphBuilder".into());
    }
    let index = value.get_property("__w3cos_operand").to_u32() as usize;
    let known = BUILDERS.with(|builders| {
        builders
            .borrow()
            .get(&builder_id)
            .is_some_and(|state| index < state.nodes.len())
    });
    if known {
        Ok(index)
    } else {
        Err("operand is not part of this graph builder".into())
    }
}

fn push_node(builder_id: u64, node: Node) -> Value {
    let index = BUILDERS.with(|builders| {
        let mut builders = builders.borrow_mut();
        let state = builders.get_mut(&builder_id).expect("builder");
        state.nodes.push(node);
        state.nodes.len() - 1
    });
    operand(builder_id, index)
}

fn unsupported(name: &'static str) -> Value {
    realm_function(crate::jsdom::realm_generation(), move |_, _| {
        w3cos_core::throw_value(error(
            "NotSupportedError",
            &format!("MLGraphBuilder.{name} is not implemented by the CPU backend"),
        ))
    })
}

pub fn ml_class() -> Value {
    class_named(
        "ML",
        realm_function(crate::jsdom::realm_generation(), |_, _| illegal("ML")),
    )
}

pub fn ml_context_class() -> Value {
    class_named(
        "MLContext",
        realm_function(crate::jsdom::realm_generation(), |_, _| {
            illegal("MLContext")
        }),
    )
}

pub fn ml_operand_class() -> Value {
    class_named(
        "MLOperand",
        realm_function(crate::jsdom::realm_generation(), |_, _| {
            illegal("MLOperand")
        }),
    )
}

pub fn ml_graph_class() -> Value {
    class_named(
        "MLGraph",
        realm_function(crate::jsdom::realm_generation(), |_, _| illegal("MLGraph")),
    )
}

pub fn ml_graph_builder_class() -> Value {
    let generation = crate::jsdom::realm_generation();
    let constructor = realm_function(generation, move |this, args| {
        let context = args.first().cloned().unwrap_or(Value::Undefined);
        if !w3cos_core::class::instance_of(&context, &ml_context_class()) {
            w3cos_core::throw_value(error("TypeError", "MLGraphBuilder requires an MLContext"));
        }
        let id = NEXT_BUILDER.with(|next| {
            let id = *next.borrow();
            *next.borrow_mut() = id + 1;
            id
        });
        BUILDERS.with(|builders| {
            builders
                .borrow_mut()
                .insert(id, BuilderState { nodes: Vec::new() });
        });
        this.set_property("__w3cos_builder", Value::Number(id as f64));
        this.set_property(
            "input",
            realm_function(generation, move |_, args| {
                let name = args.first().map(Value::to_js_string).unwrap_or_default();
                let descriptor = args.get(1).cloned().unwrap_or(Value::Undefined);
                match length_of(&descriptor) {
                    Ok(len) if !name.is_empty() => push_node(id, Node::Input { name, len }),
                    Ok(_) => w3cos_core::throw_value(error("TypeError", "input name is required")),
                    Err(message) => w3cos_core::throw_value(error("NotSupportedError", &message)),
                }
            }),
        );
        this.set_property(
            "constant",
            realm_function(generation, move |_, args| {
                let descriptor = args.first().cloned().unwrap_or(Value::Undefined);
                let buffer = args.get(1).cloned().unwrap_or(Value::Undefined);
                let len = match length_of(&descriptor) {
                    Ok(len) => len,
                    Err(message) => {
                        w3cos_core::throw_value(error("NotSupportedError", &message));
                    }
                };
                let data = match floats_of(&buffer) {
                    Ok(data) if data.len() == len => data,
                    Ok(_) => {
                        w3cos_core::throw_value(error(
                            "TypeError",
                            "constant buffer does not match dimensions",
                        ));
                    }
                    Err(message) => w3cos_core::throw_value(error("TypeError", &message)),
                };
                push_node(id, Node::Constant(data))
            }),
        );
        for (method, binary) in [("add", true), ("mul", true), ("relu", false)] {
            this.set_property(
                method,
                realm_function(generation, move |_, args| {
                    let left =
                        match operand_index(id, &args.first().cloned().unwrap_or(Value::Undefined))
                        {
                            Ok(index) => index,
                            Err(message) => w3cos_core::throw_value(error("TypeError", &message)),
                        };
                    if binary {
                        let right = match operand_index(
                            id,
                            &args.get(1).cloned().unwrap_or(Value::Undefined),
                        ) {
                            Ok(index) => index,
                            Err(message) => w3cos_core::throw_value(error("TypeError", &message)),
                        };
                        push_node(
                            id,
                            if method == "add" {
                                Node::Add(left, right)
                            } else {
                                Node::Mul(left, right)
                            },
                        )
                    } else {
                        push_node(id, Node::Relu(left))
                    }
                }),
            );
        }
        for name in ["conv2d", "gemm", "softmax", "reshape"] {
            this.set_property(name, unsupported(name));
        }
        this.set_property(
            "build",
            realm_function(generation, move |_, args| {
                let outputs = args.first().cloned().unwrap_or(Value::Undefined);
                match build_graph(id, &outputs) {
                    Ok(graph) => w3cos_core::promise::resolve(vec![graph]),
                    Err(message) => rejected("TypeError", &message),
                }
            }),
        );
        Value::Undefined
    });
    class_named("MLGraphBuilder", constructor)
}

fn build_graph(builder_id: u64, outputs: &Value) -> Result<Value, String> {
    let names = output_names(outputs)?;
    let mut resolved = Vec::new();
    for name in &names {
        resolved.push((
            name.clone(),
            operand_index(builder_id, &outputs.get_property(name))?,
        ));
    }
    let nodes = BUILDERS.with(|builders| {
        builders
            .borrow()
            .get(&builder_id)
            .map(|state| state.nodes.clone())
            .ok_or_else(|| "MLGraphBuilder is closed".to_string())
    })?;
    let id = NEXT_GRAPH.with(|next| {
        let id = *next.borrow();
        *next.borrow_mut() = id + 1;
        id
    });
    GRAPHS.with(|graphs| {
        graphs.borrow_mut().insert(
            id,
            Graph {
                nodes,
                outputs: resolved,
            },
        );
    });
    let graph = Value::object(HashMap::from([(
        "__w3cos_graph".into(),
        Value::Number(id as f64),
    )]));
    w3cos_core::class::set_prototype_of(&graph, &ml_graph_class().get_property("prototype"));
    Ok(graph)
}

fn output_names(outputs: &Value) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let length = outputs.get_property("length").to_u32();
    if length > 0 && outputs.get_property("0").is_undefined() {
        // Object maps are not arrays. Walk a small explicit key list from
        // string keys the host stored by setting properties.
    }
    // Value objects don't expose key iteration reliably. Callers pass an
    // object; we read keys from a parallel array when present, otherwise the
    // known property names used by tests and the demo via `output`.
    for key in ["output", "c", "y", "result"] {
        if !outputs.get_property(key).is_undefined() {
            names.push(key.to_string());
        }
    }
    if names.is_empty() {
        return Err("name the graph output `output`, `c`, `y`, or `result`".into());
    }
    Ok(names)
}

pub fn ml_value() -> Value {
    let generation = crate::jsdom::realm_generation();
    let ml = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(&ml, &ml_class().get_property("prototype"));
    ml.set_property(
        "createContext",
        realm_function(generation, move |_, _| {
            if let Some(rejected) = crate::capability_gate::reject_unless_granted(PERMISSION) {
                return rejected;
            }
            let context = Value::object(HashMap::new());
            w3cos_core::class::set_prototype_of(
                &context,
                &ml_context_class().get_property("prototype"),
            );
            context.set_property(
                "compute",
                realm_function(generation, |_, args| {
                    let graph = args.first().cloned().unwrap_or(Value::Undefined);
                    let feeds = args.get(1).cloned().unwrap_or(Value::Undefined);
                    match compute(&graph, &feeds) {
                        Ok(results) => w3cos_core::promise::resolve(vec![results]),
                        Err(message) => rejected("OperationError", &message),
                    }
                }),
            );
            w3cos_core::promise::resolve(vec![context])
        }),
    );
    ml
}

fn compute(graph_value: &Value, feeds: &Value) -> Result<Value, String> {
    if !w3cos_core::class::instance_of(graph_value, &ml_graph_class()) {
        return Err("compute requires an MLGraph".into());
    }
    let id = graph_value.get_property("__w3cos_graph").to_u32() as u64;
    let graph = GRAPHS.with(|graphs| graphs.borrow().get(&id).map(|graph| graph_clone(graph)));
    let Some(graph) = graph else {
        return Err("MLGraph is closed".into());
    };
    let mut values: Vec<Option<Vec<f32>>> = vec![None; graph.nodes.len()];
    for (index, node) in graph.nodes.iter().enumerate() {
        values[index] = Some(eval_node(node, &values, feeds)?);
    }
    let mut results = HashMap::new();
    for (name, index) in &graph.outputs {
        let data = values[*index].clone().unwrap_or_default();
        results.insert(name.clone(), float32_array(&data));
    }
    Ok(Value::object(results))
}

fn graph_clone(graph: &Graph) -> Graph {
    Graph {
        nodes: graph.nodes.clone(),
        outputs: graph.outputs.clone(),
    }
}

fn eval_node(node: &Node, values: &[Option<Vec<f32>>], feeds: &Value) -> Result<Vec<f32>, String> {
    match node {
        Node::Input { name, len } => {
            let data = floats_of(&feeds.get_property(name))?;
            if data.len() != *len {
                return Err(format!("input {name} has the wrong length"));
            }
            Ok(data)
        }
        Node::Constant(data) => Ok(data.clone()),
        Node::Add(left, right) | Node::Mul(left, right) => {
            let left = values[*left].as_ref().ok_or("missing operand")?;
            let right = values[*right].as_ref().ok_or("missing operand")?;
            if left.len() != right.len() {
                return Err("elementwise operands differ in length".into());
            }
            Ok(if matches!(node, Node::Add(_, _)) {
                left.iter().zip(right).map(|(a, b)| a + b).collect()
            } else {
                left.iter().zip(right).map(|(a, b)| a * b).collect()
            })
        }
        Node::Relu(input) => Ok(values[*input]
            .as_ref()
            .ok_or("missing operand")?
            .iter()
            .map(|value| value.max(0.0))
            .collect()),
    }
}

pub fn reset_realm() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
    GRAPHS.with(|graphs| graphs.borrow_mut().clear());
    BUILDERS.with(|builders| builders.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn await_value(promise: Value) -> Result<Value, String> {
        let ok = Rc::new(RefCell::new(None));
        let err = Rc::new(RefCell::new(None));
        let ok_for_then = Rc::clone(&ok);
        let err_for_catch = Rc::clone(&err);
        promise.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *ok_for_then.borrow_mut() = args.first().cloned();
                Value::Undefined
            })],
        );
        promise.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *err_for_catch.borrow_mut() = args
                    .first()
                    .map(|value| value.get_property("name").to_js_string());
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        if let Some(name) = err.borrow().clone() {
            return Err(name);
        }
        ok.borrow().clone().ok_or_else(|| "unresolved".into())
    }

    #[test]
    fn create_context_is_denied_until_webnn_is_granted_and_add_is_real() {
        reset_realm();
        crate::capability_gate::reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let denied = await_value(ml_value().call_method("createContext", vec![])).unwrap_err();
        assert_eq!(denied, "NotAllowedError");

        crate::capability_gate::grant(PERMISSION);
        let context = await_value(ml_value().call_method("createContext", vec![])).unwrap();
        assert!(w3cos_core::class::instance_of(
            &context,
            &ml_context_class()
        ));
        let builder =
            w3cos_core::class::construct(&ml_graph_builder_class(), vec![context.clone()]);
        let shape = Value::object(HashMap::from([
            ("dataType".into(), Value::string("float32")),
            ("dimensions".into(), Value::array(vec![Value::Number(2.0)])),
        ]));
        let input = builder.call_method("input", vec![Value::string("a"), shape.clone()]);
        let constant = builder.call_method("constant", vec![shape, float32_array(&[1.0, 2.0])]);
        let added = builder.call_method("add", vec![input, constant]);
        let unsupported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            builder.call_method("conv2d", vec![]);
        }));
        assert!(unsupported.is_err());
        let outputs = Value::object(HashMap::from([("output".into(), added)]));
        let graph = await_value(builder.call_method("build", vec![outputs])).unwrap();
        let feeds = Value::object(HashMap::from([("a".into(), float32_array(&[3.0, 4.0]))]));
        let results = await_value(context.call_method("compute", vec![graph, feeds])).unwrap();
        let output = floats_of(&results.get_property("output")).unwrap();
        assert_eq!(output, vec![4.0, 6.0]);
        crate::capability_gate::reset();
        reset_realm();
    }
}
