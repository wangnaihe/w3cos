//! WebGL shader and program objects backed by [`crate::webgl_shader`].
//!
//! Each rendering context owns its shader/program table (captured by the
//! context's methods, so it is released with the realm). Objects carry a
//! hidden `__w3cos_gl_id`; an object from another context is rejected with
//! `INVALID_OPERATION` as in browsers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

use crate::webgl_shader::{self, CompiledShader, LinkedProgram, Stage, Variable};
use crate::webgl_web::{class_for, object_value};

const NO_ERROR: u32 = 0;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;

const FRAGMENT_SHADER: u32 = 0x8B30;
const VERTEX_SHADER: u32 = 0x8B31;
const DELETE_STATUS: u32 = 0x8B80;
const COMPILE_STATUS: u32 = 0x8B81;
const LINK_STATUS: u32 = 0x8B82;
const VALIDATE_STATUS: u32 = 0x8B83;
const ATTACHED_SHADERS: u32 = 0x8B85;
const ACTIVE_UNIFORMS: u32 = 0x8B86;
const ACTIVE_ATTRIBUTES: u32 = 0x8B89;
const SHADER_TYPE: u32 = 0x8B4F;
const TRANSFORM_FEEDBACK_BUFFER_MODE: u32 = 0x8C7F;
const TRANSFORM_FEEDBACK_VARYINGS: u32 = 0x8C83;
const ACTIVE_UNIFORM_BLOCKS: u32 = 0x8A36;
const INTERLEAVED_ATTRIBS: u32 = 0x8C8C;

/// `MAX_VERTEX_ATTRIBS` reported for the native renderer.
pub const MAX_VERTEX_ATTRIBS: u32 = 16;

const ID_KEY: &str = "__w3cos_gl_id";

struct ShaderEntry {
    stage: Stage,
    compiled: Option<CompiledShader>,
    deleted: bool,
}

#[derive(Default)]
struct ProgramEntry {
    vertex: Option<(u64, Value)>,
    fragment: Option<(u64, Value)>,
    bound: HashMap<String, u32>,
    linked: Option<LinkedProgram>,
    validated: bool,
    deleted: bool,
}

#[derive(Default)]
struct Table {
    next_id: u64,
    shaders: HashMap<u64, ShaderEntry>,
    programs: HashMap<u64, ProgramEntry>,
    error: u32,
}

impl Table {
    fn record(&mut self, error: u32) {
        if self.error == NO_ERROR {
            self.error = error;
        }
    }
}

type Shared = Rc<RefCell<Table>>;

fn id_of(value: Option<&Value>) -> Option<u64> {
    let id = value?.get_property(ID_KEY);
    id.is_number().then(|| id.to_number() as u64)
}

fn arg_u32(args: &[Value], index: usize) -> u32 {
    args.get(index).map(Value::to_u32).unwrap_or(0)
}

fn arg_string(args: &[Value], index: usize) -> String {
    args.get(index).map(Value::to_js_string).unwrap_or_default()
}

fn method(
    target: &Value,
    name: &str,
    table: &Shared,
    callback: impl Fn(&mut Table, &[Value]) -> Value + 'static,
) {
    let table = table.clone();
    target.set_property(
        name,
        crate::jsdom::realm_function(crate::jsdom::realm_generation(), move |_, args| {
            callback(&mut table.borrow_mut(), &args)
        }),
    );
}

fn active_info(name: &str, gl_type: u32, size: u32) -> Value {
    let info = Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("type".into(), Value::Number(gl_type as f64)),
        ("size".into(), Value::Number(size as f64)),
    ]));
    w3cos_core::class::set_prototype_of(
        &info,
        &class_for("WebGLActiveInfo").get_property("prototype"),
    );
    info
}

fn reported_name(variable: &Variable, is_uniform: bool) -> String {
    if is_uniform && (variable.size > 1 || variable.name.ends_with(']')) {
        format!("{}[0]", variable.name)
    } else {
        variable.name.clone()
    }
}

/// Resolves `name`, `name[0]`, `name[i]` and struct-member names against a
/// linked uniform list.
fn find_uniform<'a>(uniforms: &'a [Variable], query: &str) -> Option<&'a Variable> {
    let (base, index) = match query
        .strip_suffix(']')
        .and_then(|rest| rest.rsplit_once('['))
    {
        Some((base, index)) => (base, index.parse::<u32>().ok()),
        None => (query, Some(0)),
    };
    if let Some(found) = uniforms.iter().find(|uniform| uniform.name == base) {
        return (index? < found.size.max(1) && found.gl_type != 0).then_some(found);
    }
    // `uniform Light u_light;` → `u_light.color`, `u_lights[1].color`.
    let head = query.split('.').next()?;
    let head = head.split('[').next()?;
    uniforms
        .iter()
        .find(|uniform| uniform.name == head && uniform.gl_type == 0 && query.contains('.'))
}

/// Installs the shader/program methods on a WebGL context object.
pub fn install(context: &Value, webgl2: bool, state: Rc<RefCell<HashMap<String, Value>>>) {
    let table: Shared = Rc::new(RefCell::new(Table::default()));

    method(context, "createShader", &table, |table, args| {
        let stage = match arg_u32(args, 0) {
            VERTEX_SHADER => Stage::Vertex,
            FRAGMENT_SHADER => Stage::Fragment,
            _ => {
                table.record(INVALID_ENUM);
                return Value::Null;
            }
        };
        table.next_id += 1;
        let id = table.next_id;
        table.shaders.insert(
            id,
            ShaderEntry {
                stage,
                compiled: None,
                deleted: false,
            },
        );
        let shader = object_value("WebGLShader");
        shader.set_property(ID_KEY, Value::Number(id as f64));
        shader
    });
    method(context, "createProgram", &table, |table, _| {
        table.next_id += 1;
        let id = table.next_id;
        table.programs.insert(id, ProgramEntry::default());
        let program = object_value("WebGLProgram");
        program.set_property(ID_KEY, Value::Number(id as f64));
        program
    });
    method(context, "compileShader", &table, move |table, args| {
        let source = args
            .first()
            .map(|shader| shader.get_property("__w3cos_source"))
            .filter(|source| source.is_string())
            .map(|source| source.to_js_string())
            .unwrap_or_default();
        let Some(entry) = id_of(args.first()).and_then(|id| table.shaders.get_mut(&id)) else {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        };
        entry.compiled = Some(webgl_shader::compile(entry.stage, &source, webgl2));
        Value::Undefined
    });
    method(context, "getShaderParameter", &table, |table, args| {
        let Some(entry) = id_of(args.first()).and_then(|id| table.shaders.get(&id)) else {
            table.record(INVALID_OPERATION);
            return Value::Null;
        };
        match arg_u32(args, 1) {
            COMPILE_STATUS => Value::Bool(entry.compiled.as_ref().is_some_and(|c| c.success)),
            SHADER_TYPE => Value::Number(match entry.stage {
                Stage::Vertex => VERTEX_SHADER,
                Stage::Fragment => FRAGMENT_SHADER,
            } as f64),
            DELETE_STATUS => Value::Bool(entry.deleted),
            _ => {
                table.record(INVALID_ENUM);
                Value::Null
            }
        }
    });
    method(
        context,
        "getShaderInfoLog",
        &table,
        |table, args| match id_of(args.first()).and_then(|id| table.shaders.get(&id)) {
            Some(entry) => Value::string(
                entry
                    .compiled
                    .as_ref()
                    .map(|compiled| compiled.info_log.as_str())
                    .unwrap_or(""),
            ),
            None => {
                table.record(INVALID_OPERATION);
                Value::Null
            }
        },
    );
    method(context, "deleteShader", &table, |table, args| {
        if let Some(shader) = args.first() {
            shader.set_property("__w3cos_deleted", Value::Bool(true));
        }
        if let Some(entry) = id_of(args.first()).and_then(|id| table.shaders.get_mut(&id)) {
            entry.deleted = true;
        }
        Value::Undefined
    });
    method(context, "attachShader", &table, |table, args| {
        let (Some(program_id), Some(shader_id)) = (id_of(args.first()), id_of(args.get(1))) else {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        };
        let Some(stage) = table.shaders.get(&shader_id).map(|entry| entry.stage) else {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        };
        let Some(program) = table.programs.get_mut(&program_id) else {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        };
        let slot = match stage {
            Stage::Vertex => &mut program.vertex,
            Stage::Fragment => &mut program.fragment,
        };
        if slot.is_some() {
            table.record(INVALID_OPERATION);
        } else {
            *slot = Some((shader_id, args[1].clone()));
        }
        Value::Undefined
    });
    method(context, "detachShader", &table, |table, args| {
        let (Some(program_id), Some(shader_id)) = (id_of(args.first()), id_of(args.get(1))) else {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        };
        let Some(program) = table.programs.get_mut(&program_id) else {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        };
        if program
            .vertex
            .as_ref()
            .is_some_and(|(id, _)| *id == shader_id)
        {
            program.vertex = None;
        } else if program
            .fragment
            .as_ref()
            .is_some_and(|(id, _)| *id == shader_id)
        {
            program.fragment = None;
        } else {
            table.record(INVALID_OPERATION);
        }
        Value::Undefined
    });
    method(
        context,
        "getAttachedShaders",
        &table,
        |table, args| match id_of(args.first()).and_then(|id| table.programs.get(&id)) {
            Some(program) => Value::array(
                program
                    .vertex
                    .iter()
                    .chain(program.fragment.iter())
                    .map(|(_, shader)| shader.clone())
                    .collect(),
            ),
            None => {
                table.record(INVALID_OPERATION);
                Value::Null
            }
        },
    );
    method(context, "bindAttribLocation", &table, |table, args| {
        let index = arg_u32(args, 1);
        let name = arg_string(args, 2);
        if index >= MAX_VERTEX_ATTRIBS {
            table.record(INVALID_VALUE);
            return Value::Undefined;
        }
        if name.starts_with("webgl_") || name.starts_with("_webgl_") || name.starts_with("gl_") {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        }
        match id_of(args.first()).and_then(|id| table.programs.get_mut(&id)) {
            Some(program) => {
                program.bound.insert(name, index);
            }
            None => table.record(INVALID_OPERATION),
        }
        Value::Undefined
    });
    method(context, "linkProgram", &table, |table, args| {
        let Some(program_id) = id_of(args.first()).filter(|id| table.programs.contains_key(id))
        else {
            table.record(INVALID_OPERATION);
            return Value::Undefined;
        };
        let program = &table.programs[&program_id];
        let compiled = |slot: &Option<(u64, Value)>| {
            slot.as_ref()
                .and_then(|(id, _)| table.shaders.get(id))
                .and_then(|entry| entry.compiled.clone())
        };
        let vertex = compiled(&program.vertex);
        let fragment = compiled(&program.fragment);
        let linked = webgl_shader::link(
            vertex.as_ref(),
            fragment.as_ref(),
            &program.bound,
            MAX_VERTEX_ATTRIBS,
        );
        let program = table.programs.get_mut(&program_id).expect("checked above");
        program.linked = Some(linked);
        program.validated = false;
        Value::Undefined
    });
    method(context, "validateProgram", &table, |table, args| {
        match id_of(args.first()).and_then(|id| table.programs.get_mut(&id)) {
            Some(program) => {
                program.validated = program.linked.as_ref().is_some_and(|linked| linked.success)
            }
            None => table.record(INVALID_OPERATION),
        }
        Value::Undefined
    });
    method(
        context,
        "getProgramParameter",
        &table,
        move |table, args| {
            let Some(program) = id_of(args.first()).and_then(|id| table.programs.get(&id)) else {
                table.record(INVALID_OPERATION);
                return Value::Null;
            };
            let linked = program.linked.as_ref().filter(|linked| linked.success);
            match arg_u32(args, 1) {
                LINK_STATUS => Value::Bool(linked.is_some()),
                DELETE_STATUS => Value::Bool(program.deleted),
                VALIDATE_STATUS => Value::Bool(program.validated),
                ATTACHED_SHADERS => Value::Number(
                    (program.vertex.is_some() as u32 + program.fragment.is_some() as u32) as f64,
                ),
                ACTIVE_ATTRIBUTES => {
                    Value::Number(linked.map_or(0, |linked| linked.attributes.len()) as f64)
                }
                ACTIVE_UNIFORMS => {
                    Value::Number(linked.map_or(0, |linked| linked.uniforms.len()) as f64)
                }
                TRANSFORM_FEEDBACK_BUFFER_MODE if webgl2 => {
                    Value::Number(INTERLEAVED_ATTRIBS as f64)
                }
                TRANSFORM_FEEDBACK_VARYINGS | ACTIVE_UNIFORM_BLOCKS if webgl2 => Value::Number(0.0),
                _ => {
                    table.record(INVALID_ENUM);
                    Value::Null
                }
            }
        },
    );
    method(
        context,
        "getProgramInfoLog",
        &table,
        |table, args| match id_of(args.first()).and_then(|id| table.programs.get(&id)) {
            Some(program) => Value::string(
                program
                    .linked
                    .as_ref()
                    .map(|linked| linked.info_log.as_str())
                    .unwrap_or(""),
            ),
            None => {
                table.record(INVALID_OPERATION);
                Value::Null
            }
        },
    );
    method(context, "deleteProgram", &table, |table, args| {
        if let Some(program) = args.first() {
            program.set_property("__w3cos_deleted", Value::Bool(true));
        }
        if let Some(entry) = id_of(args.first()).and_then(|id| table.programs.get_mut(&id)) {
            entry.deleted = true;
        }
        Value::Undefined
    });
    method(context, "getAttribLocation", &table, |table, args| {
        let name = arg_string(args, 1);
        let Some(program) = id_of(args.first()).and_then(|id| table.programs.get(&id)) else {
            table.record(INVALID_OPERATION);
            return Value::Number(-1.0);
        };
        let Some(linked) = program.linked.as_ref().filter(|linked| linked.success) else {
            table.record(INVALID_OPERATION);
            return Value::Number(-1.0);
        };
        Value::Number(
            linked
                .attributes
                .iter()
                .find(|(attribute, _)| attribute.name == name)
                .map_or(-1.0, |(_, location)| *location as f64),
        )
    });
    method(context, "getUniformLocation", &table, |table, args| {
        let name = arg_string(args, 1);
        let Some(program_id) = id_of(args.first()) else {
            table.record(INVALID_OPERATION);
            return Value::Null;
        };
        let Some(linked) = table
            .programs
            .get(&program_id)
            .and_then(|program| program.linked.as_ref())
            .filter(|linked| linked.success)
        else {
            table.record(INVALID_OPERATION);
            return Value::Null;
        };
        if find_uniform(&linked.uniforms, &name).is_none() {
            return Value::Null;
        }
        let location = object_value("WebGLUniformLocation");
        location.set_property("__w3cos_uniform_name", Value::string(&name));
        location.set_property("__w3cos_program_id", Value::Number(program_id as f64));
        location
    });
    for (name, is_uniform) in [("getActiveAttrib", false), ("getActiveUniform", true)] {
        method(context, name, &table, move |table, args| {
            let index = arg_u32(args, 1) as usize;
            let Some(program) = id_of(args.first()).and_then(|id| table.programs.get(&id)) else {
                table.record(INVALID_OPERATION);
                return Value::Null;
            };
            let linked = program.linked.as_ref().filter(|linked| linked.success);
            let variable = linked.and_then(|linked| {
                if is_uniform {
                    linked.uniforms.get(index).cloned()
                } else {
                    linked
                        .attributes
                        .get(index)
                        .map(|(attribute, _)| attribute.clone())
                }
            });
            match variable {
                Some(variable) => active_info(
                    &reported_name(&variable, is_uniform),
                    variable.gl_type,
                    variable.size.max(1),
                ),
                None => {
                    table.record(INVALID_VALUE);
                    Value::Null
                }
            }
        });
    }
    let state_for_use = state;
    method(context, "useProgram", &table, move |table, args| {
        let program = args.first().cloned().unwrap_or(Value::Null);
        if program.is_null() || program.is_undefined() {
            state_for_use.borrow_mut().remove("currentProgram");
            return Value::Undefined;
        }
        let linked = id_of(Some(&program))
            .and_then(|id| table.programs.get(&id))
            .and_then(|entry| entry.linked.as_ref())
            .is_some_and(|linked| linked.success);
        if linked {
            state_for_use
                .borrow_mut()
                .insert("currentProgram".into(), program);
        } else {
            table.record(INVALID_OPERATION);
        }
        Value::Undefined
    });
    method(context, "getError", &table, |table, _| {
        Value::Number(std::mem::replace(&mut table.error, NO_ERROR) as f64)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS: &str = "attribute vec3 a_position;\nattribute vec2 a_uv;\nuniform mat4 u_mvp;\n\
                      uniform vec2 u_offsets[3];\nvarying vec2 v_uv;\n\
                      void main(){ v_uv = a_uv + u_offsets[2]; gl_Position = u_mvp * vec4(a_position, 1.0); }\n";
    const FS: &str = "precision mediump float;\nuniform sampler2D u_tex;\nvarying vec2 v_uv;\n\
                      void main(){ gl_FragColor = texture2D(u_tex, v_uv); }\n";

    fn context(webgl2: bool) -> Value {
        crate::webgl_web::reset();
        crate::webgl_web::context_value(Value::object(HashMap::new()), webgl2)
    }

    fn shader(gl: &Value, kind: u32, source: &str) -> Value {
        let shader = gl.call_method("createShader", vec![Value::Number(kind as f64)]);
        gl.call_method("shaderSource", vec![shader.clone(), Value::string(source)]);
        gl.call_method("compileShader", vec![shader.clone()]);
        shader
    }

    fn param(gl: &Value, method: &str, object: &Value, pname: u32) -> Value {
        gl.call_method(method, vec![object.clone(), Value::Number(pname as f64)])
    }

    #[test]
    fn compile_link_and_introspect_a_textured_program() {
        let gl = context(false);
        let vs = shader(&gl, VERTEX_SHADER, VS);
        let fs = shader(&gl, FRAGMENT_SHADER, FS);
        assert!(param(&gl, "getShaderParameter", &vs, COMPILE_STATUS).to_bool());
        assert_eq!(
            param(&gl, "getShaderParameter", &fs, SHADER_TYPE).to_number(),
            FRAGMENT_SHADER as f64
        );
        let program = gl.call_method("createProgram", vec![]);
        gl.call_method("attachShader", vec![program.clone(), vs.clone()]);
        gl.call_method("attachShader", vec![program.clone(), fs.clone()]);
        gl.call_method(
            "bindAttribLocation",
            vec![program.clone(), Value::Number(3.0), Value::string("a_uv")],
        );
        gl.call_method("linkProgram", vec![program.clone()]);
        assert!(
            param(&gl, "getProgramParameter", &program, LINK_STATUS).to_bool(),
            "{}",
            gl.call_method("getProgramInfoLog", vec![program.clone()])
                .to_js_string()
        );
        assert_eq!(
            param(&gl, "getProgramParameter", &program, ATTACHED_SHADERS).to_number(),
            2.0
        );
        assert_eq!(
            param(&gl, "getProgramParameter", &program, ACTIVE_ATTRIBUTES).to_number(),
            2.0
        );
        assert_eq!(
            param(&gl, "getProgramParameter", &program, ACTIVE_UNIFORMS).to_number(),
            3.0
        );
        let location = |name: &str| {
            gl.call_method(
                "getAttribLocation",
                vec![program.clone(), Value::string(name)],
            )
            .to_number()
        };
        assert_eq!(location("a_uv"), 3.0);
        assert_eq!(location("a_position"), 0.0);
        assert_eq!(location("missing"), -1.0);
        let uniform = |name: &str| {
            gl.call_method(
                "getUniformLocation",
                vec![program.clone(), Value::string(name)],
            )
        };
        assert!(uniform("u_mvp").is_object());
        assert!(uniform("u_offsets[2]").is_object());
        assert!(uniform("u_offsets[3]").is_null());
        assert!(uniform("u_tex").is_object());
        assert!(uniform("u_missing").is_null());
        let info = gl.call_method(
            "getActiveUniform",
            vec![program.clone(), Value::Number(1.0)],
        );
        assert_eq!(info.get_property("name").to_js_string(), "u_offsets[0]");
        assert_eq!(info.get_property("size").to_number(), 3.0);
        assert_eq!(info.get_property("type").to_number(), 0x8B50 as f64);
        assert!(w3cos_core::class::instance_of(
            &info,
            &class_for("WebGLActiveInfo")
        ));
        let sampler = gl.call_method(
            "getActiveUniform",
            vec![program.clone(), Value::Number(2.0)],
        );
        assert_eq!(sampler.get_property("type").to_number(), 0x8B5E as f64);
        assert_eq!(
            gl.call_method("getAttachedShaders", vec![program.clone()])
                .get_property("length")
                .to_number(),
            2.0
        );
        gl.call_method("useProgram", vec![program.clone()]);
        assert!(
            gl.call_method("getParameter", vec![Value::Number(35725.0)])
                .strict_eq(&program)
        );
        assert_eq!(gl.call_method("getError", vec![]).to_number(), 0.0);
    }

    #[test]
    #[cfg(feature = "web-graphics-advanced")]
    fn compile_errors_surface_in_info_log_and_block_linking() {
        let gl = context(false);
        let bad = shader(
            &gl,
            FRAGMENT_SHADER,
            "precision mediump float;\nvoid main() {\n  gl_FragColor = vec4(nope);\n}\n",
        );
        assert!(!param(&gl, "getShaderParameter", &bad, COMPILE_STATUS).to_bool());
        let log = gl
            .call_method("getShaderInfoLog", vec![bad.clone()])
            .to_js_string();
        assert!(log.starts_with("ERROR: 0:3:"), "{log}");
        let vs = shader(&gl, VERTEX_SHADER, VS);
        let program = gl.call_method("createProgram", vec![]);
        gl.call_method("attachShader", vec![program.clone(), vs]);
        gl.call_method("attachShader", vec![program.clone(), bad]);
        gl.call_method("linkProgram", vec![program.clone()]);
        assert!(!param(&gl, "getProgramParameter", &program, LINK_STATUS).to_bool());
        assert!(
            !gl.call_method("getProgramInfoLog", vec![program.clone()])
                .to_js_string()
                .is_empty()
        );
        gl.call_method("useProgram", vec![program.clone()]);
        assert_eq!(
            gl.call_method("getError", vec![]).to_number(),
            INVALID_OPERATION as f64
        );
        assert_eq!(gl.call_method("getError", vec![]).to_number(), 0.0);
        assert_eq!(
            gl.call_method("getAttribLocation", vec![program, Value::string("a_uv")])
                .to_number(),
            -1.0
        );
    }

    #[test]
    fn invalid_enums_duplicate_attachments_and_foreign_objects_set_errors() {
        let gl = context(true);
        assert!(
            gl.call_method("createShader", vec![Value::Number(1.0)])
                .is_null()
        );
        assert_eq!(
            gl.call_method("getError", vec![]).to_number(),
            INVALID_ENUM as f64
        );
        let vs = shader(&gl, VERTEX_SHADER, VS);
        let program = gl.call_method("createProgram", vec![]);
        gl.call_method("attachShader", vec![program.clone(), vs.clone()]);
        gl.call_method("attachShader", vec![program.clone(), vs.clone()]);
        assert_eq!(
            gl.call_method("getError", vec![]).to_number(),
            INVALID_OPERATION as f64
        );
        gl.call_method(
            "bindAttribLocation",
            vec![program.clone(), Value::Number(99.0), Value::string("a")],
        );
        assert_eq!(
            gl.call_method("getError", vec![]).to_number(),
            INVALID_VALUE as f64
        );

        let other = crate::webgl_web::context_value(Value::object(HashMap::new()), true);
        other.call_method("compileShader", vec![vs.clone()]);
        assert_eq!(
            other.call_method("getError", vec![]).to_number(),
            INVALID_OPERATION as f64
        );

        let es3 = shader(
            &gl,
            VERTEX_SHADER,
            "#version 300 es\nin vec4 p;\nvoid main(){ gl_Position = p; }\n",
        );
        assert!(param(&gl, "getShaderParameter", &es3, COMPILE_STATUS).to_bool());
        assert_eq!(
            param(
                &gl,
                "getProgramParameter",
                &program,
                TRANSFORM_FEEDBACK_BUFFER_MODE
            )
            .to_number(),
            INTERLEAVED_ATTRIBS as f64
        );
    }
}
