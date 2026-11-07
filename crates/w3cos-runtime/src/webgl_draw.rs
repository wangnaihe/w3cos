//! WebGL buffer, texture, uniform and draw execution on wgpu.
//!
//! CPU state (object identity, bindings, uniforms, pixel unpack) always
//! updates. Pixels are produced only by a real wgpu render pass. When no
//! adapter can be opened, draw and `readPixels` record `INVALID_OPERATION`
//! and leave the destination untouched.
//!
//! Linked GLSL 450 from [`crate::webgl_shader`] is lowered to WGSL with naga.
//! OpenGL clip Z is mapped into wgpu's 0..1 range. Viewport and scissor
//! origins are converted from WebGL's bottom-left to wgpu's top-left.
//! `readPixels` returns bottom-left rows; the canvas snapshot published to
//! the compositor stays top-down, matching GPU row 0.

#![allow(clippy::too_many_arguments)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use w3cos_core::Value;

use crate::webgl_shader::{LinkedGpu, Variable};
use crate::webgl_web::{class_for, object_value};

const NO_ERROR: u32 = 0;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;
const INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;

const DEPTH_BUFFER_BIT: u32 = 0x0100;
const STENCIL_BUFFER_BIT: u32 = 0x0400;
const COLOR_BUFFER_BIT: u32 = 0x4000;

const POINTS: u32 = 0x0000;
const LINES: u32 = 0x0001;
const LINE_LOOP: u32 = 0x0002;
const LINE_STRIP: u32 = 0x0003;
const TRIANGLES: u32 = 0x0004;
const TRIANGLE_STRIP: u32 = 0x0005;
const TRIANGLE_FAN: u32 = 0x0006;

const ZERO: u32 = 0;
const ONE: u32 = 1;
const SRC_COLOR: u32 = 0x0300;
const ONE_MINUS_SRC_COLOR: u32 = 0x0301;
const SRC_ALPHA: u32 = 0x0302;
const ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const DST_ALPHA: u32 = 0x0304;
const ONE_MINUS_DST_ALPHA: u32 = 0x0305;
const DST_COLOR: u32 = 0x0306;
const ONE_MINUS_DST_COLOR: u32 = 0x0307;
const SRC_ALPHA_SATURATE: u32 = 0x0308;
const CONSTANT_COLOR: u32 = 0x8001;
const ONE_MINUS_CONSTANT_COLOR: u32 = 0x8002;
const CONSTANT_ALPHA: u32 = 0x8003;
const ONE_MINUS_CONSTANT_ALPHA: u32 = 0x8004;
const FUNC_ADD: u32 = 0x8006;
const BLEND_EQUATION: u32 = 0x8009;
const FUNC_SUBTRACT: u32 = 0x800A;
const FUNC_REVERSE_SUBTRACT: u32 = 0x800B;
const BLEND_DST_RGB: u32 = 0x80C8;
const BLEND_SRC_RGB: u32 = 0x80C9;
const BLEND_DST_ALPHA: u32 = 0x80CA;
const BLEND_SRC_ALPHA: u32 = 0x80CB;
const BLEND_EQUATION_ALPHA: u32 = 0x883D;
const MIN: u32 = 0x8007;
const MAX: u32 = 0x8008;

const NEVER: u32 = 0x0200;
const LESS: u32 = 0x0201;
const EQUAL: u32 = 0x0202;
const LEQUAL: u32 = 0x0203;
const GREATER: u32 = 0x0204;
const NOTEQUAL: u32 = 0x0205;
const GEQUAL: u32 = 0x0206;
const ALWAYS: u32 = 0x0207;

const FRONT: u32 = 0x0404;
const BACK: u32 = 0x0405;
const FRONT_AND_BACK: u32 = 0x0408;
const CW: u32 = 0x0900;
const CCW: u32 = 0x0901;

const CULL_FACE: u32 = 0x0B44;
const CULL_FACE_MODE: u32 = 0x0B45;
const FRONT_FACE: u32 = 0x0B46;
const DEPTH_TEST: u32 = 0x0B71;
const DEPTH_WRITEMASK: u32 = 0x0B72;
const DEPTH_CLEAR_VALUE: u32 = 0x0B73;
const DEPTH_FUNC: u32 = 0x0B74;
const STENCIL_TEST: u32 = 0x0B90;
const VIEWPORT: u32 = 0x0BA2;
const DITHER: u32 = 0x0BD0;
const BLEND: u32 = 0x0BE2;
const SCISSOR_TEST: u32 = 0x0C11;
const SCISSOR_BOX: u32 = 0x0C10;
const COLOR_CLEAR_VALUE: u32 = 0x0C22;
const COLOR_WRITEMASK: u32 = 0x0C23;
const UNPACK_ALIGNMENT: u32 = 0x0CF5;
const PACK_ALIGNMENT: u32 = 0x0D05;
const MAX_TEXTURE_SIZE: u32 = 0x0D33;
const MAX_VIEWPORT_DIMS: u32 = 0x0D3A;
const TEXTURE_2D: u32 = 0x0DE1;
const DEPTH_COMPONENT: u32 = 0x1902;
const ALPHA: u32 = 0x1906;
const RGB: u32 = 0x1907;
const RGBA: u32 = 0x1908;
const LUMINANCE: u32 = 0x1909;
const LUMINANCE_ALPHA: u32 = 0x190A;
const BYTE: u32 = 0x1400;
const UNSIGNED_BYTE: u32 = 0x1401;
const SHORT: u32 = 0x1402;
const UNSIGNED_SHORT: u32 = 0x1403;
const INT: u32 = 0x1404;
const UNSIGNED_INT: u32 = 0x1405;
const FLOAT: u32 = 0x1406;
const VENDOR: u32 = 0x1F00;
const RENDERER: u32 = 0x1F01;
const VERSION: u32 = 0x1F02;
const NEAREST: u32 = 0x2600;
const LINEAR: u32 = 0x2601;
const NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
const LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
const NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
const LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
const REPEAT: u32 = 0x2901;
const CLAMP_TO_EDGE: u32 = 0x812F;
const MIRRORED_REPEAT: u32 = 0x8370;
const TEXTURE_MAG_FILTER: u32 = 0x2800;
const TEXTURE_MIN_FILTER: u32 = 0x2801;
const TEXTURE_WRAP_S: u32 = 0x2802;
const TEXTURE_WRAP_T: u32 = 0x2803;
const ARRAY_BUFFER: u32 = 0x8892;
const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const ARRAY_BUFFER_BINDING: u32 = 0x8894;
const ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
const BUFFER_SIZE: u32 = 0x8764;
const BUFFER_USAGE: u32 = 0x8765;
const STREAM_DRAW: u32 = 0x88E0;
const STATIC_DRAW: u32 = 0x88E4;
const DYNAMIC_DRAW: u32 = 0x88E8;
const VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;
const VERTEX_ATTRIB_ARRAY_SIZE: u32 = 0x8623;
const VERTEX_ATTRIB_ARRAY_STRIDE: u32 = 0x8624;
const VERTEX_ATTRIB_ARRAY_TYPE: u32 = 0x8625;
const CURRENT_VERTEX_ATTRIB: u32 = 0x8626;
const VERTEX_ATTRIB_ARRAY_POINTER: u32 = 0x8645;
const VERTEX_ATTRIB_ARRAY_NORMALIZED: u32 = 0x886A;
const VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: u32 = 0x889F;
const VERTEX_ATTRIB_ARRAY_DIVISOR: u32 = 0x88FE;
const MAX_VERTEX_ATTRIBS_PNAME: u32 = 0x8869;
const CURRENT_PROGRAM: u32 = 0x8B8D;
const SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;
const FRAMEBUFFER: u32 = 0x8D40;
const RENDERBUFFER: u32 = 0x8D41;
const READ_FRAMEBUFFER: u32 = 0x8CA8;
const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const RENDERBUFFER_BINDING: u32 = 0x8CA7;
const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT: u32 = 0x8CD7;
const FRAMEBUFFER_INCOMPLETE_DIMENSIONS: u32 = 0x8CD9;
const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const DEPTH_ATTACHMENT: u32 = 0x8D00;
const TEXTURE0: u32 = 0x84C0;
const TEXTURE_BINDING_2D: u32 = 0x8069;
const ACTIVE_TEXTURE: u32 = 0x84E0;
const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;
const UNPACK_PREMULTIPLY_ALPHA_WEBGL: u32 = 0x9241;
const UNPACK_ROW_LENGTH: u32 = 0x0CF2;
const UNPACK_SKIP_ROWS: u32 = 0x0CF3;
const UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
const PACK_ROW_LENGTH: u32 = 0x0D02;
const POLYGON_OFFSET_FILL: u32 = 0x8037;
const SAMPLE_ALPHA_TO_COVERAGE: u32 = 0x809E;
const VERTEX_ARRAY_BINDING: u32 = 0x85B5;
const SAMPLER_2D: u32 = 0x8B5E;
const RGBA8: u32 = 0x8058;
const RGB8: u32 = 0x8051;
const DEPTH_COMPONENT16: u32 = 0x81A5;

const ID_KEY: &str = "__w3cos_gl_id";
const CTX_KEY: &str = "__w3cos_gl_ctx";
const MAX_ATTRIBS: usize = 16;
const MAX_TEXTURE_UNITS: usize = 16;

static NEXT_CTX: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static LIVE: RefCell<Vec<Weak<RefCell<DrawState>>>> = RefCell::new(Vec::new());
    static WARNED: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());
}

fn warn_once(key: &str, message: &str) {
    let fresh = WARNED.with(|seen| seen.borrow_mut().insert(key.to_string()));
    if fresh {
        eprintln!("[w3cos] warning: {message}");
    }
}

/// True when a WebGL context still owns draw state on this thread.
pub fn has_live_contexts() -> bool {
    LIVE.with(|live| live.borrow().iter().any(|weak| weak.strong_count() > 0))
}

/// Publish dirty default framebuffers whose canvas DOM id appears in `nodes`.
///
/// `nodes` are `(paint client index, DOM node id)` pairs from the flattened
/// paint tree. Nothing is published when the GPU did not render.
pub fn publish_drawing_buffers(nodes: impl IntoIterator<Item = (usize, u32)>) {
    let nodes: Vec<(usize, u32)> = nodes.into_iter().collect();
    if nodes.is_empty() {
        return;
    }
    LIVE.with(|live| {
        live.borrow_mut().retain(|weak| weak.strong_count() > 0);
        let weaks = live.borrow().clone();
        for weak in weaks {
            let Some(state) = weak.upgrade() else {
                continue;
            };
            let mut state = state.borrow_mut();
            let Some(node) = state.node_id else {
                continue;
            };
            if let Some((index, _)) = nodes.iter().find(|(_, id)| *id == node) {
                state.publish(*index);
            }
        }
    });
}

#[derive(Clone)]
struct Attrib {
    enabled: bool,
    buffer: Option<u64>,
    size: u32,
    type_: u32,
    normalized: bool,
    stride: u32,
    offset: u32,
    divisor: u32,
    current: [f32; 4],
}

impl Default for Attrib {
    fn default() -> Self {
        Self {
            enabled: false,
            buffer: None,
            size: 4,
            type_: FLOAT,
            normalized: false,
            stride: 0,
            offset: 0,
            divisor: 0,
            current: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

struct Vao {
    attribs: [Attrib; MAX_ATTRIBS],
    element: Value,
}

impl Default for Vao {
    fn default() -> Self {
        Self {
            attribs: std::array::from_fn(|_| Attrib::default()),
            element: Value::Null,
        }
    }
}

struct Buffer {
    bytes: Vec<u8>,
    usage: u32,
}

struct Texture {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    min_filter: u32,
    mag_filter: u32,
    wrap_s: u32,
    wrap_t: u32,
    mips_requested: bool,
    gpu_dirty: bool,
}

impl Default for Texture {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            min_filter: NEAREST_MIPMAP_LINEAR,
            mag_filter: LINEAR,
            wrap_s: REPEAT,
            wrap_t: REPEAT,
            mips_requested: false,
            gpu_dirty: true,
        }
    }
}

struct RenderbufferObj {
    width: u32,
    height: u32,
    format: u32,
}

struct FramebufferObj {
    color: Option<u64>,
    depth: Option<u64>,
}

#[derive(Clone)]
struct StoredUniform {
    bytes: Vec<u8>,
    sampler_units: Vec<u32>,
}

struct DrawState {
    ctx: u64,
    webgl2: bool,
    canvas: Value,
    node_id: Option<u32>,
    state: Rc<RefCell<HashMap<String, Value>>>,
    error: Rc<Cell<u32>>,
    programs: Rc<RefCell<HashMap<u64, LinkedGpu>>>,
    next_id: u64,
    buffers: HashMap<u64, Buffer>,
    textures: HashMap<u64, Texture>,
    renderbuffers: HashMap<u64, RenderbufferObj>,
    framebuffers: HashMap<u64, FramebufferObj>,
    vaos: HashMap<u64, Vao>,
    default_vao: Vao,
    vao: Value,
    array_buffer: Value,
    textures_bound: Vec<Value>,
    active_unit: usize,
    draw_fbo: Value,
    read_fbo: Value,
    renderbuffer: Value,
    viewport: [i32; 4],
    scissor: [i32; 4],
    scissor_on: bool,
    clear_color: [f32; 4],
    clear_depth: f32,
    depth_test: bool,
    depth_mask: bool,
    depth_func: u32,
    depth_range: [f32; 2],
    blend: bool,
    blend_src_rgb: u32,
    blend_dst_rgb: u32,
    blend_src_a: u32,
    blend_dst_a: u32,
    blend_eq_rgb: u32,
    blend_eq_a: u32,
    blend_color: [f32; 4],
    cull: bool,
    cull_mode: u32,
    front_cw: bool,
    color_mask: [bool; 4],
    dither: bool,
    polygon_offset: bool,
    polygon_factor: f32,
    polygon_units: f32,
    unpack_alignment: u32,
    pack_alignment: u32,
    unpack_flip_y: bool,
    unpack_premultiply: bool,
    unpack_row_length: u32,
    unpack_skip_rows: u32,
    unpack_skip_pixels: u32,
    pack_row_length: u32,
    uniforms: HashMap<(u64, String), StoredUniform>,
    snapshot: Option<crate::canvas2d::CanvasSnapshot>,
    snapshot_index: Option<usize>,
    dirty: bool,
    revision: u64,
    #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
    gpu: gpu::ContextGpu,
}

impl DrawState {
    fn record(&self, error: u32) {
        if error != NO_ERROR && self.error.get() == NO_ERROR {
            self.error.set(error);
        }
    }

    fn no_gpu(&self) {
        warn_once(
            "no-adapter",
            "WebGL draw/readPixels was rejected because no wgpu adapter is available",
        );
        self.record(INVALID_OPERATION);
    }

    fn publish(&mut self, index: usize) {
        #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
        gpu::with_parked(self, |draw| {
            if draw.dirty && gpu::readback_default(draw).is_err() {
                return;
            }
            if draw.snapshot_index == Some(index) && !draw.dirty {
                return;
            }
            if let Some(snapshot) = draw.snapshot.clone() {
                crate::canvas2d::publish_surface(index, snapshot);
                draw.snapshot_index = Some(index);
            }
        });
        #[cfg(not(all(feature = "gpu", feature = "web-graphics-advanced")))]
        {
            let _ = index;
        }
    }

    fn canvas_size(&self) -> (u32, u32) {
        let dim = |name: &str, default: u32| {
            let value = self.canvas.get_property(name).to_number();
            if !value.is_finite() {
                default
            } else if value <= 0.0 {
                0
            } else {
                (value as u32).min(4096)
            }
        };
        (dim("width", 300), dim("height", 150))
    }

    fn vao_mut(&mut self) -> &mut Vao {
        if let Some(id) = bound_id(&self.vao) {
            self.vaos.entry(id).or_default()
        } else {
            &mut self.default_vao
        }
    }

    fn sync_clear_color(&self) {
        self.state.borrow_mut().insert(
            "clearColor".into(),
            Value::array(
                self.clear_color
                    .iter()
                    .map(|channel| Value::Number(*channel as f64))
                    .collect(),
            ),
        );
    }

    fn sync_viewport(&self) {
        self.state.borrow_mut().insert(
            "viewport".into(),
            Value::array(
                self.viewport
                    .iter()
                    .map(|channel| Value::Number(*channel as f64))
                    .collect(),
            ),
        );
    }
}

#[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
impl Drop for DrawState {
    fn drop(&mut self) {
        // Resources live in the per-thread park between calls. Drop them
        // here while this thread can still take wgpu's snatch lock. If the
        // park was already destroyed, it dropped them in the right order.
        gpu::release_parked(self.ctx);
    }
}

fn bound_id(value: &Value) -> Option<u64> {
    if value.is_null() || value.is_undefined() {
        return None;
    }
    let id = value.get_property(ID_KEY).to_number();
    (id > 0.0).then_some(id as u64)
}

fn classify(value: &Value, class_name: &'static str, ctx: u64) -> Result<Option<u64>, u32> {
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    if !w3cos_core::class::instance_of(value, &class_for(class_name))
        || value.get_property("__w3cos_deleted").to_bool()
    {
        return Err(INVALID_OPERATION);
    }
    if value.get_property(CTX_KEY).to_number() as u64 != ctx {
        return Err(INVALID_OPERATION);
    }
    bound_id(value).map(Some).ok_or(INVALID_OPERATION)
}

fn new_object(ctx: u64, id: u64, class_name: &'static str) -> Value {
    let object = object_value(class_name);
    object.set_property(ID_KEY, Value::Number(id as f64));
    object.set_property(CTX_KEY, Value::Number(ctx as f64));
    object
}

type Shared = Rc<RefCell<DrawState>>;

fn set_method(
    context: &Value,
    name: &'static str,
    draw: &Shared,
    callback: impl Fn(&mut DrawState, &[Value]) -> Value + 'static,
) {
    let draw = draw.clone();
    context.set_property(
        name,
        crate::jsdom::realm_function(crate::jsdom::realm_generation(), move |_, args| {
            callback(&mut draw.borrow_mut(), &args)
        }),
    );
}

/// Installs draw, upload and readback methods on one WebGL context.
pub fn install(
    context: &Value,
    webgl2: bool,
    canvas: Value,
    state: Rc<RefCell<HashMap<String, Value>>>,
    error: Rc<Cell<u32>>,
    programs: Rc<RefCell<HashMap<u64, LinkedGpu>>>,
) {
    let (width, height) = {
        let dim = |name: &str, default: i32| {
            let value = canvas.get_property(name).to_number();
            if value.is_finite() && value >= 1.0 {
                (value as i32).clamp(1, 4096)
            } else if value.is_finite() && value == 0.0 {
                0
            } else {
                default
            }
        };
        (dim("width", 300), dim("height", 150))
    };
    let ctx = NEXT_CTX.fetch_add(1, Ordering::Relaxed);
    let draw = Rc::new(RefCell::new(DrawState {
        ctx,
        webgl2,
        node_id: crate::jsdom::node_id_of(&canvas),
        canvas,
        state: state.clone(),
        error,
        programs,
        next_id: 0,
        buffers: HashMap::new(),
        textures: HashMap::new(),
        renderbuffers: HashMap::new(),
        framebuffers: HashMap::new(),
        vaos: HashMap::new(),
        default_vao: Vao::default(),
        vao: Value::Null,
        array_buffer: Value::Null,
        textures_bound: vec![Value::Null; MAX_TEXTURE_UNITS],
        active_unit: 0,
        draw_fbo: Value::Null,
        read_fbo: Value::Null,
        renderbuffer: Value::Null,
        viewport: [0, 0, width, height],
        scissor: [0, 0, width, height],
        scissor_on: false,
        clear_color: [0.0, 0.0, 0.0, 0.0],
        clear_depth: 1.0,
        depth_test: false,
        depth_mask: true,
        depth_func: LESS,
        depth_range: [0.0, 1.0],
        blend: false,
        blend_src_rgb: ONE,
        blend_dst_rgb: ZERO,
        blend_src_a: ONE,
        blend_dst_a: ZERO,
        blend_eq_rgb: FUNC_ADD,
        blend_eq_a: FUNC_ADD,
        blend_color: [0.0, 0.0, 0.0, 0.0],
        cull: false,
        cull_mode: BACK,
        front_cw: false,
        color_mask: [true, true, true, true],
        dither: true,
        polygon_offset: false,
        polygon_factor: 0.0,
        polygon_units: 0.0,
        unpack_alignment: 4,
        pack_alignment: 4,
        unpack_flip_y: false,
        unpack_premultiply: false,
        unpack_row_length: 0,
        unpack_skip_rows: 0,
        unpack_skip_pixels: 0,
        pack_row_length: 0,
        uniforms: HashMap::new(),
        snapshot: None,
        snapshot_index: None,
        dirty: false,
        revision: 0,
        #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
        gpu: gpu::ContextGpu::default(),
    }));
    draw.borrow().sync_viewport();
    LIVE.with(|live| live.borrow_mut().push(Rc::downgrade(&draw)));

    for (name, class_name) in [
        ("createBuffer", "WebGLBuffer"),
        ("createTexture", "WebGLTexture"),
        ("createFramebuffer", "WebGLFramebuffer"),
        ("createRenderbuffer", "WebGLRenderbuffer"),
        ("createVertexArray", "WebGLVertexArrayObject"),
    ] {
        set_method(context, name, &draw, move |draw, _| {
            draw.next_id += 1;
            let id = draw.next_id;
            match class_name {
                "WebGLBuffer" => {
                    draw.buffers.insert(
                        id,
                        Buffer {
                            bytes: Vec::new(),
                            usage: STATIC_DRAW,
                        },
                    );
                }
                "WebGLTexture" => {
                    draw.textures.insert(id, Texture::default());
                }
                "WebGLFramebuffer" => {
                    draw.framebuffers.insert(
                        id,
                        FramebufferObj {
                            color: None,
                            depth: None,
                        },
                    );
                }
                "WebGLRenderbuffer" => {
                    draw.renderbuffers.insert(
                        id,
                        RenderbufferObj {
                            width: 0,
                            height: 0,
                            format: RGBA,
                        },
                    );
                }
                _ => {
                    draw.vaos.insert(id, Vao::default());
                }
            }
            new_object(draw.ctx, id, class_name)
        });
    }
    for (name, class_name) in [
        ("deleteBuffer", "WebGLBuffer"),
        ("deleteTexture", "WebGLTexture"),
        ("deleteFramebuffer", "WebGLFramebuffer"),
        ("deleteRenderbuffer", "WebGLRenderbuffer"),
        ("deleteVertexArray", "WebGLVertexArrayObject"),
    ] {
        set_method(context, name, &draw, move |draw, args| {
            let Some(object) = args.first() else {
                return Value::Undefined;
            };
            object.set_property("__w3cos_deleted", Value::Bool(true));
            if let Ok(Some(id)) = classify(object, class_name, draw.ctx) {
                match class_name {
                    "WebGLBuffer" => {
                        draw.buffers.remove(&id);
                        if bound_id(&draw.array_buffer) == Some(id) {
                            draw.array_buffer = Value::Null;
                        }
                        let vao = draw.vao_mut();
                        if bound_id(&vao.element) == Some(id) {
                            vao.element = Value::Null;
                        }
                    }
                    "WebGLTexture" => {
                        draw.textures.remove(&id);
                        for slot in &mut draw.textures_bound {
                            if bound_id(slot) == Some(id) {
                                *slot = Value::Null;
                            }
                        }
                    }
                    "WebGLFramebuffer" => {
                        draw.framebuffers.remove(&id);
                        if bound_id(&draw.draw_fbo) == Some(id) {
                            draw.draw_fbo = Value::Null;
                        }
                        if bound_id(&draw.read_fbo) == Some(id) {
                            draw.read_fbo = Value::Null;
                        }
                    }
                    "WebGLRenderbuffer" => {
                        draw.renderbuffers.remove(&id);
                        if bound_id(&draw.renderbuffer) == Some(id) {
                            draw.renderbuffer = Value::Null;
                        }
                    }
                    _ => {
                        draw.vaos.remove(&id);
                        if bound_id(&draw.vao) == Some(id) {
                            draw.vao = Value::Null;
                        }
                    }
                }
            }
            Value::Undefined
        });
    }

    set_method(context, "bindBuffer", &draw, |draw, args| {
        let target = arg_u32(args, 0);
        let object = args.get(1).cloned().unwrap_or(Value::Null);
        match classify(&object, "WebGLBuffer", draw.ctx) {
            Ok(id) => {
                if id.is_some() && !draw.buffers.contains_key(&id.unwrap()) {
                    draw.record(INVALID_OPERATION);
                    return Value::Undefined;
                }
                match target {
                    ARRAY_BUFFER => draw.array_buffer = object,
                    ELEMENT_ARRAY_BUFFER => draw.vao_mut().element = object,
                    _ => draw.record(INVALID_ENUM),
                }
            }
            Err(error) => draw.record(error),
        }
        Value::Undefined
    });
    set_method(context, "bufferData", &draw, |draw, args| {
        draw.buffer_data(args);
        Value::Undefined
    });
    set_method(context, "bufferSubData", &draw, |draw, args| {
        draw.buffer_sub_data(args);
        Value::Undefined
    });
    set_method(context, "getBufferParameter", &draw, |draw, args| {
        draw.buffer_parameter(arg_u32(args, 0), arg_u32(args, 1))
    });
    if webgl2 {
        set_method(context, "getBufferSubData", &draw, |draw, args| {
            draw.buffer_sub_data_read(args);
            Value::Undefined
        });
        set_method(context, "copyBufferSubData", &draw, |draw, args| {
            draw.copy_buffer_sub_data(args);
            Value::Undefined
        });
    }

    set_method(context, "enableVertexAttribArray", &draw, |draw, args| {
        draw.attrib_enabled(arg_u32(args, 0), true);
        Value::Undefined
    });
    set_method(context, "disableVertexAttribArray", &draw, |draw, args| {
        draw.attrib_enabled(arg_u32(args, 0), false);
        Value::Undefined
    });
    set_method(context, "vertexAttribPointer", &draw, |draw, args| {
        draw.attrib_pointer(args, false);
        Value::Undefined
    });
    for name in [
        "vertexAttrib1f",
        "vertexAttrib2f",
        "vertexAttrib3f",
        "vertexAttrib4f",
        "vertexAttrib1fv",
        "vertexAttrib2fv",
        "vertexAttrib3fv",
        "vertexAttrib4fv",
    ] {
        set_method(context, name, &draw, move |draw, args| {
            draw.attrib_value(name, args);
            Value::Undefined
        });
    }
    set_method(context, "getVertexAttrib", &draw, |draw, args| {
        draw.vertex_attrib(arg_u32(args, 0), arg_u32(args, 1))
    });
    set_method(context, "getVertexAttribOffset", &draw, |draw, args| {
        if arg_u32(args, 1) != VERTEX_ATTRIB_ARRAY_POINTER {
            draw.record(INVALID_ENUM);
            return Value::Number(0.0);
        }
        let index = arg_u32(args, 0) as usize;
        if index >= MAX_ATTRIBS {
            draw.record(INVALID_VALUE);
            return Value::Number(0.0);
        }
        Value::Number(draw.vao_mut().attribs[index].offset as f64)
    });
    if webgl2 {
        set_method(context, "vertexAttribDivisor", &draw, |draw, args| {
            let index = arg_u32(args, 0) as usize;
            if index >= MAX_ATTRIBS {
                draw.record(INVALID_VALUE);
            } else {
                draw.vao_mut().attribs[index].divisor = arg_u32(args, 1);
            }
            Value::Undefined
        });
        set_method(context, "vertexAttribIPointer", &draw, |draw, args| {
            draw.attrib_pointer(args, true);
            Value::Undefined
        });
        set_method(context, "bindVertexArray", &draw, |draw, args| {
            let object = args.first().cloned().unwrap_or(Value::Null);
            match classify(&object, "WebGLVertexArrayObject", draw.ctx) {
                Ok(Some(id)) if draw.vaos.contains_key(&id) => draw.vao = object,
                Ok(None) => draw.vao = Value::Null,
                Ok(Some(_)) | Err(_) => draw.record(INVALID_OPERATION),
            }
            Value::Undefined
        });
    }

    for name in [
        "uniform1f",
        "uniform1fv",
        "uniform1i",
        "uniform1iv",
        "uniform1ui",
        "uniform1uiv",
        "uniform2f",
        "uniform2fv",
        "uniform2i",
        "uniform2iv",
        "uniform2ui",
        "uniform2uiv",
        "uniform3f",
        "uniform3fv",
        "uniform3i",
        "uniform3iv",
        "uniform3ui",
        "uniform3uiv",
        "uniform4f",
        "uniform4fv",
        "uniform4i",
        "uniform4iv",
        "uniform4ui",
        "uniform4uiv",
        "uniformMatrix2fv",
        "uniformMatrix3fv",
        "uniformMatrix4fv",
        "uniformMatrix2x3fv",
        "uniformMatrix2x4fv",
        "uniformMatrix3x2fv",
        "uniformMatrix3x4fv",
        "uniformMatrix4x2fv",
        "uniformMatrix4x3fv",
    ] {
        set_method(context, name, &draw, move |draw, args| {
            draw.uniform(name, args);
            Value::Undefined
        });
    }
    set_method(context, "getUniform", &draw, |draw, args| {
        draw.get_uniform(args)
    });

    set_method(context, "activeTexture", &draw, |draw, args| {
        let unit = arg_u32(args, 0);
        if (TEXTURE0..TEXTURE0 + MAX_TEXTURE_UNITS as u32).contains(&unit) {
            draw.active_unit = (unit - TEXTURE0) as usize;
        } else {
            draw.record(INVALID_ENUM);
        }
        Value::Undefined
    });
    set_method(context, "bindTexture", &draw, |draw, args| {
        if arg_u32(args, 0) != TEXTURE_2D {
            warn_once(
                "texture-target",
                "WebGL bindTexture only executes GL_TEXTURE_2D on the wgpu compositor",
            );
            draw.record(INVALID_ENUM);
            return Value::Undefined;
        }
        let object = args.get(1).cloned().unwrap_or(Value::Null);
        match classify(&object, "WebGLTexture", draw.ctx) {
            Ok(Some(id)) if draw.textures.contains_key(&id) => {
                draw.textures_bound[draw.active_unit] = object;
            }
            Ok(None) => draw.textures_bound[draw.active_unit] = Value::Null,
            Ok(Some(_)) | Err(_) => draw.record(INVALID_OPERATION),
        }
        Value::Undefined
    });
    set_method(context, "texImage2D", &draw, |draw, args| {
        draw.tex_image(args, false);
        Value::Undefined
    });
    set_method(context, "texSubImage2D", &draw, |draw, args| {
        draw.tex_image(args, true);
        Value::Undefined
    });
    for name in ["texParameteri", "texParameterf"] {
        set_method(context, name, &draw, |draw, args| {
            draw.tex_parameter(arg_u32(args, 1), arg_u32(args, 2));
            Value::Undefined
        });
    }
    set_method(context, "getTexParameter", &draw, |draw, args| {
        draw.tex_parameter_get(arg_u32(args, 1))
    });
    set_method(context, "generateMipmap", &draw, |draw, _| {
        warn_once(
            "mipmap",
            "WebGL generateMipmap does not build extra levels; sampling uses level 0",
        );
        if let Some(texture) = draw.bound_texture_mut() {
            if texture.width == 0 || texture.height == 0 {
                draw.record(INVALID_OPERATION);
            } else {
                texture.mips_requested = true;
            }
        } else {
            draw.record(INVALID_OPERATION);
        }
        Value::Undefined
    });
    set_method(context, "pixelStorei", &draw, |draw, args| {
        draw.pixel_store(
            arg_u32(args, 0),
            args.get(1).map(Value::to_i32).unwrap_or(0),
        );
        Value::Undefined
    });

    set_method(context, "bindFramebuffer", &draw, |draw, args| {
        let target = arg_u32(args, 0);
        let object = args.get(1).cloned().unwrap_or(Value::Null);
        if !matches!(target, FRAMEBUFFER | DRAW_FRAMEBUFFER | READ_FRAMEBUFFER) {
            draw.record(INVALID_ENUM);
            return Value::Undefined;
        }
        match classify(&object, "WebGLFramebuffer", draw.ctx) {
            Ok(Some(id)) if draw.framebuffers.contains_key(&id) => {
                if target != READ_FRAMEBUFFER {
                    draw.draw_fbo = object.clone();
                }
                if target != DRAW_FRAMEBUFFER {
                    draw.read_fbo = object;
                }
            }
            Ok(None) => {
                if target != READ_FRAMEBUFFER {
                    draw.draw_fbo = Value::Null;
                }
                if target != DRAW_FRAMEBUFFER {
                    draw.read_fbo = Value::Null;
                }
            }
            Ok(Some(_)) | Err(_) => draw.record(INVALID_OPERATION),
        }
        Value::Undefined
    });
    set_method(context, "bindRenderbuffer", &draw, |draw, args| {
        if arg_u32(args, 0) != RENDERBUFFER {
            draw.record(INVALID_ENUM);
            return Value::Undefined;
        }
        let object = args.get(1).cloned().unwrap_or(Value::Null);
        match classify(&object, "WebGLRenderbuffer", draw.ctx) {
            Ok(Some(id)) if draw.renderbuffers.contains_key(&id) => draw.renderbuffer = object,
            Ok(None) => draw.renderbuffer = Value::Null,
            Ok(Some(_)) | Err(_) => draw.record(INVALID_OPERATION),
        }
        Value::Undefined
    });
    set_method(context, "renderbufferStorage", &draw, |draw, args| {
        draw.renderbuffer_storage(args);
        Value::Undefined
    });
    set_method(context, "framebufferTexture2D", &draw, |draw, args| {
        draw.framebuffer_texture(args);
        Value::Undefined
    });
    set_method(context, "framebufferRenderbuffer", &draw, |draw, args| {
        draw.framebuffer_renderbuffer(args);
        Value::Undefined
    });
    set_method(context, "checkFramebufferStatus", &draw, |draw, args| {
        Value::Number(draw.framebuffer_status(arg_u32(args, 0)) as f64)
    });

    set_method(context, "viewport", &draw, |draw, args| {
        draw.viewport = [
            args.get(0).map(Value::to_i32).unwrap_or(0),
            args.get(1).map(Value::to_i32).unwrap_or(0),
            args.get(2).map(Value::to_i32).unwrap_or(0).max(0),
            args.get(3).map(Value::to_i32).unwrap_or(0).max(0),
        ];
        draw.sync_viewport();
        Value::Undefined
    });
    set_method(context, "scissor", &draw, |draw, args| {
        draw.scissor = [
            args.get(0).map(Value::to_i32).unwrap_or(0),
            args.get(1).map(Value::to_i32).unwrap_or(0),
            args.get(2).map(Value::to_i32).unwrap_or(0).max(0),
            args.get(3).map(Value::to_i32).unwrap_or(0).max(0),
        ];
        Value::Undefined
    });
    set_method(context, "clearColor", &draw, |draw, args| {
        for (index, channel) in draw.clear_color.iter_mut().enumerate() {
            *channel = args.get(index).map(Value::to_number).unwrap_or(0.0) as f32;
        }
        draw.sync_clear_color();
        Value::Undefined
    });
    set_method(context, "clearDepth", &draw, |draw, args| {
        draw.clear_depth = args
            .first()
            .map(Value::to_number)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0) as f32;
        Value::Undefined
    });
    set_method(context, "clear", &draw, |draw, args| {
        draw.clear(arg_u32(args, 0));
        Value::Undefined
    });
    set_method(context, "enable", &draw, |draw, args| {
        draw.set_cap(arg_u32(args, 0), true);
        Value::Undefined
    });
    set_method(context, "disable", &draw, |draw, args| {
        draw.set_cap(arg_u32(args, 0), false);
        Value::Undefined
    });
    set_method(context, "isEnabled", &draw, |draw, args| {
        Value::Bool(draw.cap_enabled(arg_u32(args, 0)))
    });
    set_method(context, "blendFunc", &draw, |draw, args| {
        let (src, dst) = (arg_u32(args, 0), arg_u32(args, 1));
        if blend_factor_ok(src) && blend_factor_ok(dst) {
            draw.blend_src_rgb = src;
            draw.blend_src_a = src;
            draw.blend_dst_rgb = dst;
            draw.blend_dst_a = dst;
        } else {
            draw.record(INVALID_ENUM);
        }
        Value::Undefined
    });
    set_method(context, "blendFuncSeparate", &draw, |draw, args| {
        let values = [
            arg_u32(args, 0),
            arg_u32(args, 1),
            arg_u32(args, 2),
            arg_u32(args, 3),
        ];
        if values.iter().all(|value| blend_factor_ok(*value)) {
            draw.blend_src_rgb = values[0];
            draw.blend_dst_rgb = values[1];
            draw.blend_src_a = values[2];
            draw.blend_dst_a = values[3];
        } else {
            draw.record(INVALID_ENUM);
        }
        Value::Undefined
    });
    set_method(context, "blendEquation", &draw, |draw, args| {
        let equation = arg_u32(args, 0);
        if blend_equation_ok(equation) {
            draw.blend_eq_rgb = equation;
            draw.blend_eq_a = equation;
        } else {
            draw.record(INVALID_ENUM);
        }
        Value::Undefined
    });
    set_method(context, "blendEquationSeparate", &draw, |draw, args| {
        let rgb = arg_u32(args, 0);
        let alpha = arg_u32(args, 1);
        if blend_equation_ok(rgb) && blend_equation_ok(alpha) {
            draw.blend_eq_rgb = rgb;
            draw.blend_eq_a = alpha;
        } else {
            draw.record(INVALID_ENUM);
        }
        Value::Undefined
    });
    set_method(context, "blendColor", &draw, |draw, args| {
        for (index, channel) in draw.blend_color.iter_mut().enumerate() {
            *channel = args.get(index).map(Value::to_number).unwrap_or(0.0) as f32;
        }
        Value::Undefined
    });
    set_method(context, "depthFunc", &draw, |draw, args| {
        let func = arg_u32(args, 0);
        if (NEVER..=ALWAYS).contains(&func) {
            draw.depth_func = func;
        } else {
            draw.record(INVALID_ENUM);
        }
        Value::Undefined
    });
    set_method(context, "depthMask", &draw, |draw, args| {
        draw.depth_mask = args.first().is_none_or(Value::to_bool);
        Value::Undefined
    });
    set_method(context, "depthRange", &draw, |draw, args| {
        let near = args.first().map(Value::to_number).unwrap_or(0.0) as f32;
        let far = args.get(1).map(Value::to_number).unwrap_or(1.0) as f32;
        if near > far {
            draw.record(INVALID_OPERATION);
        } else {
            draw.depth_range = [near.clamp(0.0, 1.0), far.clamp(0.0, 1.0)];
        }
        Value::Undefined
    });
    set_method(context, "colorMask", &draw, |draw, args| {
        for (index, channel) in draw.color_mask.iter_mut().enumerate() {
            *channel = args.get(index).is_none_or(Value::to_bool);
        }
        Value::Undefined
    });
    set_method(context, "cullFace", &draw, |draw, args| {
        match arg_u32(args, 0) {
            mode @ (FRONT | BACK | FRONT_AND_BACK) => draw.cull_mode = mode,
            _ => draw.record(INVALID_ENUM),
        }
        Value::Undefined
    });
    set_method(context, "frontFace", &draw, |draw, args| {
        match arg_u32(args, 0) {
            CW => draw.front_cw = true,
            CCW => draw.front_cw = false,
            _ => draw.record(INVALID_ENUM),
        }
        Value::Undefined
    });
    set_method(context, "polygonOffset", &draw, |draw, args| {
        draw.polygon_factor = args.first().map(Value::to_number).unwrap_or(0.0) as f32;
        draw.polygon_units = args.get(1).map(Value::to_number).unwrap_or(0.0) as f32;
        Value::Undefined
    });
    set_method(context, "lineWidth", &draw, |draw, args| {
        let width = args.first().map(Value::to_number).unwrap_or(1.0);
        if !(width > 0.0) {
            draw.record(INVALID_VALUE);
        } else if (width - 1.0).abs() > f64::EPSILON {
            warn_once(
                "line-width",
                "WebGL lineWidth other than 1 is accepted but the wgpu rasterizer draws width 1",
            );
        }
        Value::Undefined
    });
    set_method(context, "drawArrays", &draw, |draw, args| {
        draw.draw(
            arg_u32(args, 0),
            arg_i32(args, 1),
            arg_i32(args, 2),
            1,
            None,
        );
        Value::Undefined
    });
    set_method(context, "drawElements", &draw, |draw, args| {
        draw.draw(
            arg_u32(args, 0),
            0,
            arg_i32(args, 1),
            1,
            Some((arg_u32(args, 2), arg_i32(args, 3).max(0) as u32)),
        );
        Value::Undefined
    });
    if webgl2 {
        set_method(context, "drawArraysInstanced", &draw, |draw, args| {
            draw.draw(
                arg_u32(args, 0),
                arg_i32(args, 1),
                arg_i32(args, 2),
                arg_i32(args, 3),
                None,
            );
            Value::Undefined
        });
        set_method(context, "drawElementsInstanced", &draw, |draw, args| {
            draw.draw(
                arg_u32(args, 0),
                0,
                arg_i32(args, 1),
                arg_i32(args, 4),
                Some((arg_u32(args, 2), arg_i32(args, 3).max(0) as u32)),
            );
            Value::Undefined
        });
        set_method(context, "drawRangeElements", &draw, |draw, args| {
            let start = arg_u32(args, 1);
            let end = arg_u32(args, 2);
            if end < start {
                draw.record(INVALID_VALUE);
            } else {
                draw.draw(
                    arg_u32(args, 0),
                    0,
                    arg_i32(args, 3),
                    1,
                    Some((arg_u32(args, 4), arg_i32(args, 5).max(0) as u32)),
                );
            }
            Value::Undefined
        });
    }
    set_method(context, "readPixels", &draw, |draw, args| {
        draw.read_pixels(args);
        Value::Undefined
    });
    set_method(context, "finish", &draw, |_draw, _| {
        #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
        gpu::finish();
        Value::Undefined
    });
    set_method(context, "flush", &draw, |_, _| Value::Undefined);
    set_method(context, "getParameter", &draw, |draw, args| {
        draw.parameter(arg_u32(args, 0))
    });
    for name in ["copyTexImage2D", "copyTexSubImage2D", "texStorage2D"] {
        set_method(context, name, &draw, move |draw, _| {
            warn_once(
                name,
                &format!("WebGL {name}() is not executed on the wgpu compositor and was rejected"),
            );
            draw.record(INVALID_OPERATION);
            Value::Undefined
        });
    }
}

fn arg_u32(args: &[Value], index: usize) -> u32 {
    args.get(index).map(Value::to_u32).unwrap_or(0)
}

fn arg_i32(args: &[Value], index: usize) -> i32 {
    args.get(index).map(Value::to_i32).unwrap_or(0)
}

fn buffer_usage_ok(usage: u32) -> bool {
    matches!(
        usage,
        STATIC_DRAW
            | DYNAMIC_DRAW
            | STREAM_DRAW
            | 0x88E1
            | 0x88E2
            | 0x88E5
            | 0x88E6
            | 0x88E9
            | 0x88EA
    )
}

fn blend_factor_ok(factor: u32) -> bool {
    matches!(
        factor,
        ZERO | ONE
            | SRC_COLOR
            | ONE_MINUS_SRC_COLOR
            | SRC_ALPHA
            | ONE_MINUS_SRC_ALPHA
            | DST_ALPHA
            | ONE_MINUS_DST_ALPHA
            | DST_COLOR
            | ONE_MINUS_DST_COLOR
            | SRC_ALPHA_SATURATE
            | CONSTANT_COLOR
            | ONE_MINUS_CONSTANT_COLOR
            | CONSTANT_ALPHA
            | ONE_MINUS_CONSTANT_ALPHA
    )
}

fn blend_equation_ok(equation: u32) -> bool {
    matches!(
        equation,
        FUNC_ADD | FUNC_SUBTRACT | FUNC_REVERSE_SUBTRACT | MIN | MAX
    )
}

impl DrawState {
    fn target_buffer(&mut self, target: u32) -> Result<u64, u32> {
        let value = match target {
            ARRAY_BUFFER => self.array_buffer.clone(),
            ELEMENT_ARRAY_BUFFER => self.vao_mut().element.clone(),
            _ => return Err(INVALID_ENUM),
        };
        bound_id(&value).ok_or(INVALID_OPERATION)
    }

    fn buffer_data(&mut self, args: &[Value]) {
        let target = arg_u32(args, 0);
        let usage_index = if args.len() >= 5 { 2 } else { 2 };
        let usage = arg_u32(args, usage_index);
        if !buffer_usage_ok(usage) {
            self.record(INVALID_ENUM);
            return;
        }
        let Ok(id) = self.target_buffer(target) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let source = args.get(1).cloned().unwrap_or(Value::Null);
        let mut bytes = if source.is_number() {
            let size = source.to_number();
            if size < 0.0 || size > 64.0 * 1024.0 * 1024.0 {
                self.record(INVALID_VALUE);
                return;
            }
            vec![0u8; size as usize]
        } else if source.is_null() || source.is_undefined() {
            Vec::new()
        } else if let Some(mut data) = w3cos_core::binary::bytes_of(&source) {
            if args.len() >= 5 {
                let offset = arg_u32(args, 3) as usize;
                let length = args
                    .get(4)
                    .map(Value::to_number)
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| value as usize);
                if offset > data.len() {
                    self.record(INVALID_VALUE);
                    return;
                }
                data = data[offset..].to_vec();
                if let Some(length) = length {
                    if length > data.len() {
                        self.record(INVALID_VALUE);
                        return;
                    }
                    data.truncate(length);
                }
            }
            data
        } else {
            self.record(INVALID_VALUE);
            return;
        };
        if let Some(buffer) = self.buffers.get_mut(&id) {
            buffer.usage = usage;
            buffer.bytes = std::mem::take(&mut bytes);
        }
    }

    fn buffer_sub_data(&mut self, args: &[Value]) {
        let Ok(id) = self.target_buffer(arg_u32(args, 0)) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let offset = arg_i32(args, 1);
        if offset < 0 {
            self.record(INVALID_VALUE);
            return;
        }
        let Some(data) = args.get(2).and_then(w3cos_core::binary::bytes_of) else {
            self.record(INVALID_VALUE);
            return;
        };
        let Some(buffer) = self.buffers.get_mut(&id) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let offset = offset as usize;
        if offset + data.len() > buffer.bytes.len() {
            self.record(INVALID_VALUE);
            return;
        }
        buffer.bytes[offset..offset + data.len()].copy_from_slice(&data);
    }

    fn buffer_parameter(&mut self, target: u32, pname: u32) -> Value {
        let Ok(id) = self.target_buffer(target) else {
            self.record(INVALID_OPERATION);
            return Value::Null;
        };
        let Some(buffer) = self.buffers.get(&id) else {
            self.record(INVALID_OPERATION);
            return Value::Null;
        };
        match pname {
            BUFFER_SIZE => Value::Number(buffer.bytes.len() as f64),
            BUFFER_USAGE => Value::Number(buffer.usage as f64),
            _ => {
                self.record(INVALID_ENUM);
                Value::Null
            }
        }
    }

    fn buffer_sub_data_read(&mut self, args: &[Value]) {
        let Ok(id) = self.target_buffer(arg_u32(args, 0)) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let offset = arg_i32(args, 1);
        let dst = args.get(2).cloned().unwrap_or(Value::Null);
        if offset < 0 {
            self.record(INVALID_VALUE);
            return;
        }
        let Some(buffer) = self.buffers.get(&id) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let Some(view_len) = w3cos_core::binary::bytes_of(&dst).map(|bytes| bytes.len()) else {
            self.record(INVALID_VALUE);
            return;
        };
        let offset = offset as usize;
        if offset + view_len > buffer.bytes.len() {
            self.record(INVALID_VALUE);
            return;
        }
        let _ = w3cos_core::binary::fill_array_buffer_view(
            &dst,
            &buffer.bytes[offset..offset + view_len],
        );
    }

    fn copy_buffer_sub_data(&mut self, args: &[Value]) {
        let read = arg_u32(args, 0);
        let write = arg_u32(args, 1);
        let read_offset = arg_i32(args, 2);
        let write_offset = arg_i32(args, 3);
        let size = arg_i32(args, 4);
        if read_offset < 0 || write_offset < 0 || size < 0 {
            self.record(INVALID_VALUE);
            return;
        }
        let Ok(read_id) = self.target_buffer(read) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let Ok(write_id) = self.target_buffer(write) else {
            self.record(INVALID_OPERATION);
            return;
        };
        if read_id == write_id {
            let Some(buffer) = self.buffers.get_mut(&read_id) else {
                self.record(INVALID_OPERATION);
                return;
            };
            let start = read_offset as usize;
            let end = start + size as usize;
            let dest = write_offset as usize;
            if end > buffer.bytes.len() || dest + size as usize > buffer.bytes.len() {
                self.record(INVALID_VALUE);
                return;
            }
            buffer.bytes.copy_within(start..end, dest);
            return;
        }
        let Some(source) = self
            .buffers
            .get(&read_id)
            .map(|buffer| buffer.bytes.clone())
        else {
            self.record(INVALID_OPERATION);
            return;
        };
        let Some(buffer) = self.buffers.get_mut(&write_id) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let start = read_offset as usize;
        let end = start + size as usize;
        let dest = write_offset as usize;
        if end > source.len() || dest + size as usize > buffer.bytes.len() {
            self.record(INVALID_VALUE);
            return;
        }
        buffer.bytes[dest..dest + size as usize].copy_from_slice(&source[start..end]);
    }

    fn attrib_enabled(&mut self, index: u32, enabled: bool) {
        if index as usize >= MAX_ATTRIBS {
            self.record(INVALID_VALUE);
            return;
        }
        self.vao_mut().attribs[index as usize].enabled = enabled;
    }

    fn attrib_pointer(&mut self, args: &[Value], integer: bool) {
        let index = arg_u32(args, 0) as usize;
        if index >= MAX_ATTRIBS {
            self.record(INVALID_VALUE);
            return;
        }
        let size = arg_u32(args, 1);
        let type_ = arg_u32(args, 2);
        let (normalized, stride, offset) = if integer {
            (false, arg_u32(args, 3), arg_i32(args, 4))
        } else {
            (
                args.get(3).is_some_and(Value::to_bool),
                arg_u32(args, 4),
                arg_i32(args, 5),
            )
        };
        if !(1..=4).contains(&size) || offset < 0 {
            self.record(INVALID_VALUE);
            return;
        }
        if !matches!(
            type_,
            BYTE | UNSIGNED_BYTE | SHORT | UNSIGNED_SHORT | INT | UNSIGNED_INT | FLOAT
        ) {
            self.record(INVALID_ENUM);
            return;
        }
        if integer && type_ == FLOAT {
            self.record(INVALID_ENUM);
            return;
        }
        let Some(buffer) = bound_id(&self.array_buffer) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let attrib = &mut self.vao_mut().attribs[index];
        attrib.buffer = Some(buffer);
        attrib.size = size;
        attrib.type_ = type_;
        attrib.normalized = normalized;
        attrib.stride = stride;
        attrib.offset = offset as u32;
    }

    fn attrib_value(&mut self, name: &str, args: &[Value]) {
        let index = arg_u32(args, 0) as usize;
        if index >= MAX_ATTRIBS {
            self.record(INVALID_VALUE);
            return;
        }
        let components = name
            .chars()
            .find(|ch| ch.is_ascii_digit())
            .and_then(|ch| ch.to_digit(10))
            .unwrap_or(4) as usize;
        let values = if name.ends_with('v') {
            args.get(1)
                .map(read_numbers)
                .unwrap_or_default()
                .into_iter()
                .map(|value| value as f32)
                .collect::<Vec<_>>()
        } else {
            args.iter()
                .skip(1)
                .map(|value| value.to_number() as f32)
                .collect()
        };
        let attrib = &mut self.vao_mut().attribs[index];
        for component in 0..components.min(4) {
            attrib.current[component] = values.get(component).copied().unwrap_or(0.0);
        }
    }

    fn vertex_attrib(&mut self, index: u32, pname: u32) -> Value {
        if index as usize >= MAX_ATTRIBS {
            self.record(INVALID_VALUE);
            return Value::Null;
        }
        let attrib = self.vao_mut().attribs[index as usize].clone();
        match pname {
            VERTEX_ATTRIB_ARRAY_ENABLED => Value::Bool(attrib.enabled),
            VERTEX_ATTRIB_ARRAY_SIZE => Value::Number(attrib.size as f64),
            VERTEX_ATTRIB_ARRAY_STRIDE => Value::Number(attrib.stride as f64),
            VERTEX_ATTRIB_ARRAY_TYPE => Value::Number(attrib.type_ as f64),
            VERTEX_ATTRIB_ARRAY_NORMALIZED => Value::Bool(attrib.normalized),
            VERTEX_ATTRIB_ARRAY_BUFFER_BINDING => self
                .buffers
                .get_key_value(&attrib.buffer.unwrap_or(0))
                .map(|_| Value::Number(attrib.buffer.unwrap_or(0) as f64))
                .unwrap_or(Value::Null),
            CURRENT_VERTEX_ATTRIB => Value::array(
                attrib
                    .current
                    .iter()
                    .map(|value| Value::Number(*value as f64))
                    .collect(),
            ),
            VERTEX_ATTRIB_ARRAY_DIVISOR => Value::Number(attrib.divisor as f64),
            _ => {
                self.record(INVALID_ENUM);
                Value::Null
            }
        }
    }

    fn current_program(&self) -> Option<u64> {
        let state = self.state.borrow();
        let program = state.get("currentProgram")?;
        bound_id(program)
    }

    fn linked(&self, program: u64) -> Option<LinkedGpu> {
        self.programs.borrow().get(&program).cloned()
    }

    fn uniform(&mut self, name: &str, args: &[Value]) {
        let Some(location) = args.first() else {
            return;
        };
        if location.is_null() || location.is_undefined() {
            return;
        }
        let program = location.get_property("__w3cos_program_id").to_number();
        let uniform_name = location.get_property("__w3cos_uniform_name").to_js_string();
        if program <= 0.0 || uniform_name.is_empty() {
            self.record(INVALID_OPERATION);
            return;
        }
        if self.current_program() != Some(program as u64) {
            self.record(INVALID_OPERATION);
            return;
        }
        let Some(parsed) = parse_uniform_call(name) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let Some(linked) = self.linked(program as u64) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let (base, element) = uniform_element(&uniform_name);
        let Some(variable) = linked.uniforms.iter().find(|uniform| uniform.name == base) else {
            self.record(INVALID_OPERATION);
            return;
        };
        if is_sampler(variable.gl_type) {
            if parsed.matrix || parsed.kind != ScalarKind::Int || parsed.cols != 1 {
                self.record(INVALID_OPERATION);
                return;
            }
            let values = if parsed.vector {
                read_numbers(args.get(1).unwrap_or(&Value::Null))
            } else {
                vec![args.get(1).map(Value::to_number).unwrap_or(0.0)]
            };
            let stored = self.uniform_slot(program as u64, variable);
            for (offset, value) in values.iter().enumerate() {
                let slot = element as usize + offset;
                if slot < stored.sampler_units.len() {
                    stored.sampler_units[slot] = (*value as i32).max(0) as u32;
                }
            }
            return;
        }
        let values = if parsed.vector || parsed.matrix {
            let data_index = if parsed.matrix { 2 } else { 1 };
            read_numbers(args.get(data_index).unwrap_or(&Value::Null))
        } else {
            args.iter().skip(1).map(Value::to_number).collect()
        };
        let transpose = parsed.matrix && args.get(1).is_some_and(Value::to_bool);
        if let Err(error) = write_uniform(variable, element, &parsed, transpose, &values, self) {
            self.record(error);
        }
    }

    fn uniform_slot(&mut self, program: u64, variable: &Variable) -> &mut StoredUniform {
        let size = std140_bytes(variable);
        let units = if is_sampler(variable.gl_type) {
            variable.size.max(1) as usize
        } else {
            0
        };
        self.uniforms
            .entry((program, variable.name.clone()))
            .or_insert_with(|| StoredUniform {
                bytes: vec![0u8; size],
                sampler_units: vec![0; units],
            })
    }

    fn get_uniform(&mut self, args: &[Value]) -> Value {
        let program = bound_id(args.first().unwrap_or(&Value::Null));
        let location = args.get(1).cloned().unwrap_or(Value::Null);
        if location.is_null() || location.is_undefined() {
            self.record(INVALID_OPERATION);
            return Value::Null;
        }
        let location_program = location.get_property("__w3cos_program_id").to_number() as u64;
        if program != Some(location_program) {
            self.record(INVALID_OPERATION);
            return Value::Null;
        }
        let name = location.get_property("__w3cos_uniform_name").to_js_string();
        let Some(linked) = self.linked(location_program) else {
            self.record(INVALID_OPERATION);
            return Value::Null;
        };
        let (base, element) = uniform_element(&name);
        let Some(variable) = linked
            .uniforms
            .iter()
            .find(|uniform| uniform.name == base)
            .cloned()
        else {
            self.record(INVALID_OPERATION);
            return Value::Null;
        };
        let stored = self.uniforms.get(&(location_program, base)).cloned();
        if is_sampler(variable.gl_type) {
            let unit = stored
                .as_ref()
                .and_then(|stored| stored.sampler_units.get(element as usize).copied())
                .unwrap_or(0);
            return Value::Number(unit as f64);
        }
        let bytes = stored
            .map(|stored| stored.bytes)
            .unwrap_or_else(|| vec![0u8; std140_bytes(&variable)]);
        unpack_uniform(&variable, element, &bytes)
    }

    fn bound_texture_mut(&mut self) -> Option<&mut Texture> {
        let id = bound_id(&self.textures_bound[self.active_unit])?;
        self.textures.get_mut(&id)
    }

    fn tex_parameter(&mut self, pname: u32, value: u32) {
        let Some(texture) = self.bound_texture_mut() else {
            self.record(INVALID_OPERATION);
            return;
        };
        match pname {
            TEXTURE_MIN_FILTER => {
                if matches!(
                    value,
                    NEAREST
                        | LINEAR
                        | NEAREST_MIPMAP_NEAREST
                        | LINEAR_MIPMAP_NEAREST
                        | NEAREST_MIPMAP_LINEAR
                        | LINEAR_MIPMAP_LINEAR
                ) {
                    texture.min_filter = value;
                    texture.gpu_dirty = true;
                } else {
                    self.record(INVALID_ENUM);
                }
            }
            TEXTURE_MAG_FILTER => {
                if matches!(value, NEAREST | LINEAR) {
                    texture.mag_filter = value;
                    texture.gpu_dirty = true;
                } else {
                    self.record(INVALID_ENUM);
                }
            }
            TEXTURE_WRAP_S => {
                if matches!(value, REPEAT | CLAMP_TO_EDGE | MIRRORED_REPEAT) {
                    texture.wrap_s = value;
                    texture.gpu_dirty = true;
                } else {
                    self.record(INVALID_ENUM);
                }
            }
            TEXTURE_WRAP_T => {
                if matches!(value, REPEAT | CLAMP_TO_EDGE | MIRRORED_REPEAT) {
                    texture.wrap_t = value;
                    texture.gpu_dirty = true;
                } else {
                    self.record(INVALID_ENUM);
                }
            }
            _ => {
                warn_once(
                    "tex-parameter",
                    "WebGL texParameter only executes min/mag filter and wrap S/T",
                );
                self.record(INVALID_ENUM);
            }
        }
    }

    fn tex_parameter_get(&mut self, pname: u32) -> Value {
        let Some(id) = bound_id(&self.textures_bound[self.active_unit]) else {
            self.record(INVALID_OPERATION);
            return Value::Null;
        };
        let Some(texture) = self.textures.get(&id) else {
            self.record(INVALID_OPERATION);
            return Value::Null;
        };
        match pname {
            TEXTURE_MIN_FILTER => Value::Number(texture.min_filter as f64),
            TEXTURE_MAG_FILTER => Value::Number(texture.mag_filter as f64),
            TEXTURE_WRAP_S => Value::Number(texture.wrap_s as f64),
            TEXTURE_WRAP_T => Value::Number(texture.wrap_t as f64),
            _ => {
                self.record(INVALID_ENUM);
                Value::Null
            }
        }
    }

    fn pixel_store(&mut self, pname: u32, value: i32) {
        match pname {
            UNPACK_ALIGNMENT | PACK_ALIGNMENT => {
                if !matches!(value, 1 | 2 | 4 | 8) {
                    self.record(INVALID_VALUE);
                    return;
                }
                if pname == UNPACK_ALIGNMENT {
                    self.unpack_alignment = value as u32;
                } else {
                    self.pack_alignment = value as u32;
                }
            }
            UNPACK_FLIP_Y_WEBGL => self.unpack_flip_y = value != 0,
            UNPACK_PREMULTIPLY_ALPHA_WEBGL => self.unpack_premultiply = value != 0,
            UNPACK_ROW_LENGTH => {
                if value < 0 {
                    self.record(INVALID_VALUE);
                } else {
                    self.unpack_row_length = value as u32;
                }
            }
            UNPACK_SKIP_ROWS => {
                if value < 0 {
                    self.record(INVALID_VALUE);
                } else {
                    self.unpack_skip_rows = value as u32;
                }
            }
            UNPACK_SKIP_PIXELS => {
                if value < 0 {
                    self.record(INVALID_VALUE);
                } else {
                    self.unpack_skip_pixels = value as u32;
                }
            }
            PACK_ROW_LENGTH => {
                if value < 0 {
                    self.record(INVALID_VALUE);
                } else {
                    self.pack_row_length = value as u32;
                }
            }
            _ => self.record(INVALID_ENUM),
        }
    }

    fn tex_image(&mut self, args: &[Value], sub: bool) {
        if arg_u32(args, 0) != TEXTURE_2D {
            warn_once(
                "tex-image-target",
                "WebGL texImage2D only executes GL_TEXTURE_2D on the wgpu compositor",
            );
            self.record(INVALID_ENUM);
            return;
        }
        let Some(id) = bound_id(&self.textures_bound[self.active_unit]) else {
            self.record(INVALID_OPERATION);
            return;
        };
        if !self.textures.contains_key(&id) {
            self.record(INVALID_OPERATION);
            return;
        }
        let level = arg_i32(args, 1);
        if level != 0 {
            warn_once(
                "tex-level",
                "WebGL texture uploads above level 0 are ignored; sampling uses level 0",
            );
            return;
        }
        let pixels_form = if sub {
            args.len() >= 9 || args.get(6).is_some_and(Value::is_number)
        } else {
            args.len() >= 9 || args.get(5).is_some_and(Value::is_number)
        };
        let (x, y, width, height, format, type_, source) = if sub && pixels_form {
            (
                arg_i32(args, 2),
                arg_i32(args, 3),
                arg_i32(args, 4),
                arg_i32(args, 5),
                arg_u32(args, 6),
                arg_u32(args, 7),
                args.get(8).cloned().unwrap_or(Value::Null),
            )
        } else if sub {
            let (width, height, pixels) = match self.pixels_from_source(args.get(6)) {
                Ok(value) => value,
                Err(error) => {
                    self.record(error);
                    return;
                }
            };
            (
                arg_i32(args, 2),
                arg_i32(args, 3),
                width as i32,
                height as i32,
                arg_u32(args, 4),
                arg_u32(args, 5),
                pixels,
            )
        } else if pixels_form {
            if arg_i32(args, 5) != 0 {
                self.record(INVALID_VALUE);
                return;
            }
            (
                0,
                0,
                arg_i32(args, 3),
                arg_i32(args, 4),
                arg_u32(args, 6),
                arg_u32(args, 7),
                args.get(8).cloned().unwrap_or(Value::Null),
            )
        } else {
            let (width, height, pixels) = match self.pixels_from_source(args.get(5)) {
                Ok(value) => value,
                Err(error) => {
                    self.record(error);
                    return;
                }
            };
            (
                0,
                0,
                width as i32,
                height as i32,
                arg_u32(args, 3),
                arg_u32(args, 4),
                pixels,
            )
        };
        if !sub {
            let internal = arg_u32(args, 2);
            if !format_compatible(internal, format) {
                self.record(INVALID_OPERATION);
                return;
            }
        }
        if x < 0 || y < 0 || width < 0 || height < 0 {
            self.record(INVALID_VALUE);
            return;
        }
        if type_ != UNSIGNED_BYTE {
            warn_once(
                "tex-type",
                "WebGL texImage2D only executes UNSIGNED_BYTE uploads on the wgpu compositor",
            );
            self.record(INVALID_ENUM);
            return;
        }
        let components = match format {
            RGBA => 4,
            RGB => 3,
            LUMINANCE_ALPHA => 2,
            LUMINANCE | ALPHA => 1,
            _ => {
                self.record(INVALID_ENUM);
                return;
            }
        };
        let rgba = if source.is_null() || source.is_undefined() {
            if sub {
                self.record(INVALID_VALUE);
                return;
            }
            vec![0u8; width as usize * height as usize * 4]
        } else if components_already_rgba(&source) {
            match w3cos_core::binary::bytes_of(&source) {
                Some(bytes) if bytes.len() == width as usize * height as usize * 4 => bytes,
                _ => {
                    self.record(INVALID_OPERATION);
                    return;
                }
            }
        } else {
            let Some(bytes) = w3cos_core::binary::bytes_of(&source) else {
                self.record(INVALID_VALUE);
                return;
            };
            match unpack_rgba(
                &bytes,
                width as u32,
                height as u32,
                components,
                self.unpack_alignment,
                self.unpack_row_length,
                self.unpack_skip_rows,
                self.unpack_skip_pixels,
                format == ALPHA,
            ) {
                Ok(pixels) => pixels,
                Err(error) => {
                    self.record(error);
                    return;
                }
            }
        };
        let mut rgba = rgba;
        if self.unpack_flip_y {
            flip_rows(&mut rgba, width as u32, height as u32);
        }
        if self.unpack_premultiply {
            premultiply(&mut rgba);
        }
        let texture = self.textures.get_mut(&id).expect("texture");
        if sub {
            if x as u32 + width as u32 > texture.width || y as u32 + height as u32 > texture.height
            {
                self.record(INVALID_VALUE);
                return;
            }
            blit_rgba(
                &mut texture.pixels,
                texture.width,
                &rgba,
                width as u32,
                height as u32,
                x as u32,
                y as u32,
            );
        } else {
            texture.width = width as u32;
            texture.height = height as u32;
            texture.pixels = rgba;
            texture.mips_requested = false;
        }
        texture.gpu_dirty = true;
    }

    fn pixels_from_source(&self, source: Option<&Value>) -> Result<(u32, u32, Value), u32> {
        let Some(source) = source else {
            return Err(INVALID_VALUE);
        };
        if source.is_null() || source.is_undefined() {
            return Err(INVALID_VALUE);
        }
        let data = source.get_property("data");
        let width = source.get_property("width").to_number();
        let height = source.get_property("height").to_number();
        if w3cos_core::binary::is_array_buffer_view(&data) && width > 0.0 && height > 0.0 {
            return Ok((width as u32, height as u32, data));
        }
        if let Some(node) = crate::jsdom::node_id_of(source)
            .or_else(|| crate::jsdom::node_id_of(&source.get_property("__w3cos_canvas")))
        {
            if let Some((width, height, pixels)) = crate::jsdom::canvas_2d_rgba(node) {
                return Ok((width, height, bytes_value(&pixels)));
            }
            if let Some((width, height, pixels)) = presented_canvas(node) {
                return Ok((width, height, bytes_value(&pixels)));
            }
        }
        let src = source.get_property("src").to_js_string();
        if !src.is_empty()
            && let Some(image) = crate::image_loader::get_or_load(&src)
        {
            return Ok((
                image.width,
                image.height,
                bytes_value(image.data.as_slice()),
            ));
        }
        warn_once(
            "tex-source",
            "WebGL texImage2D source has no pixels (need ImageData, canvas, ImageBitmap, or a loaded image)",
        );
        Err(INVALID_VALUE)
    }

    fn renderbuffer_storage(&mut self, args: &[Value]) {
        if arg_u32(args, 0) != RENDERBUFFER {
            self.record(INVALID_ENUM);
            return;
        }
        let Some(id) = bound_id(&self.renderbuffer) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let format = arg_u32(args, 1);
        let width = arg_i32(args, 2);
        let height = arg_i32(args, 3);
        if width < 0 || height < 0 || width > 4096 || height > 4096 {
            self.record(INVALID_VALUE);
            return;
        }
        if !matches!(
            format,
            DEPTH_COMPONENT | DEPTH_COMPONENT16 | RGBA | RGBA8 | RGB | RGB8
        ) {
            warn_once(
                "renderbuffer-format",
                "WebGL renderbufferStorage only executes RGBA8 and depth formats on the wgpu compositor",
            );
            self.record(INVALID_ENUM);
            return;
        }
        if let Some(renderbuffer) = self.renderbuffers.get_mut(&id) {
            renderbuffer.width = width as u32;
            renderbuffer.height = height as u32;
            renderbuffer.format = format;
        }
    }

    fn framebuffer_texture(&mut self, args: &[Value]) {
        let attachment = arg_u32(args, 1);
        if arg_u32(args, 2) != TEXTURE_2D || arg_i32(args, 4) != 0 {
            self.record(INVALID_VALUE);
            return;
        }
        let Some(fbo) = bound_id(&self.draw_fbo) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let texture = args.get(3).cloned().unwrap_or(Value::Null);
        let texture_id = match classify(&texture, "WebGLTexture", self.ctx) {
            Ok(id) => id,
            Err(error) => {
                self.record(error);
                return;
            }
        };
        let Some(framebuffer) = self.framebuffers.get_mut(&fbo) else {
            self.record(INVALID_OPERATION);
            return;
        };
        match attachment {
            COLOR_ATTACHMENT0 => framebuffer.color = texture_id,
            DEPTH_ATTACHMENT => {
                warn_once(
                    "depth-texture",
                    "WebGL depth texture attachments are rejected; use a depth renderbuffer",
                );
                self.record(INVALID_OPERATION);
            }
            _ => self.record(INVALID_ENUM),
        }
    }

    fn framebuffer_renderbuffer(&mut self, args: &[Value]) {
        if arg_u32(args, 2) != RENDERBUFFER {
            self.record(INVALID_ENUM);
            return;
        }
        let Some(fbo) = bound_id(&self.draw_fbo) else {
            self.record(INVALID_OPERATION);
            return;
        };
        let renderbuffer = args.get(3).cloned().unwrap_or(Value::Null);
        let id = match classify(&renderbuffer, "WebGLRenderbuffer", self.ctx) {
            Ok(id) => id,
            Err(error) => {
                self.record(error);
                return;
            }
        };
        let Some(framebuffer) = self.framebuffers.get_mut(&fbo) else {
            self.record(INVALID_OPERATION);
            return;
        };
        match arg_u32(args, 1) {
            DEPTH_ATTACHMENT => framebuffer.depth = id,
            COLOR_ATTACHMENT0 => {
                warn_once(
                    "color-renderbuffer",
                    "WebGL color renderbuffer attachments are rejected; attach a texture",
                );
                self.record(INVALID_OPERATION);
            }
            _ => self.record(INVALID_ENUM),
        }
    }

    fn framebuffer_status(&self, target: u32) -> u32 {
        if !matches!(target, FRAMEBUFFER | DRAW_FRAMEBUFFER | READ_FRAMEBUFFER) {
            return INVALID_ENUM;
        }
        let fbo = if target == READ_FRAMEBUFFER {
            &self.read_fbo
        } else {
            &self.draw_fbo
        };
        let Some(id) = bound_id(fbo) else {
            return FRAMEBUFFER_COMPLETE;
        };
        let Some(framebuffer) = self.framebuffers.get(&id) else {
            return FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        };
        if framebuffer.color.is_none() && framebuffer.depth.is_none() {
            return FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        let color = framebuffer.color.and_then(|id| self.textures.get(&id));
        let depth = framebuffer.depth.and_then(|id| self.renderbuffers.get(&id));
        if let Some(color) = color
            && (color.width == 0 || color.height == 0)
        {
            return FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        if let (Some(color), Some(depth)) = (color, depth)
            && (color.width != depth.width || color.height != depth.height)
        {
            return FRAMEBUFFER_INCOMPLETE_DIMENSIONS;
        }
        FRAMEBUFFER_COMPLETE
    }

    fn set_cap(&mut self, cap: u32, enabled: bool) {
        match cap {
            DEPTH_TEST => self.depth_test = enabled,
            BLEND => self.blend = enabled,
            CULL_FACE => self.cull = enabled,
            SCISSOR_TEST => self.scissor_on = enabled,
            DITHER => self.dither = enabled,
            POLYGON_OFFSET_FILL => self.polygon_offset = enabled,
            STENCIL_TEST | SAMPLE_ALPHA_TO_COVERAGE => warn_once(
                "cap",
                "WebGL stencil and sample-alpha caps are accepted as state but not executed on wgpu",
            ),
            _ => self.record(INVALID_ENUM),
        }
    }

    fn cap_enabled(&mut self, cap: u32) -> bool {
        match cap {
            DEPTH_TEST => self.depth_test,
            BLEND => self.blend,
            CULL_FACE => self.cull,
            SCISSOR_TEST => self.scissor_on,
            DITHER => self.dither,
            POLYGON_OFFSET_FILL => self.polygon_offset,
            _ => {
                self.record(INVALID_ENUM);
                false
            }
        }
    }

    fn parameter(&mut self, pname: u32) -> Value {
        match pname {
            COLOR_CLEAR_VALUE => Value::array(
                self.clear_color
                    .iter()
                    .map(|channel| Value::Number(*channel as f64))
                    .collect(),
            ),
            DEPTH_CLEAR_VALUE => Value::Number(self.clear_depth as f64),
            VIEWPORT => Value::array(
                self.viewport
                    .iter()
                    .map(|value| Value::Number(*value as f64))
                    .collect(),
            ),
            SCISSOR_BOX => Value::array(
                self.scissor
                    .iter()
                    .map(|value| Value::Number(*value as f64))
                    .collect(),
            ),
            CURRENT_PROGRAM => self
                .state
                .borrow()
                .get("currentProgram")
                .cloned()
                .unwrap_or(Value::Null),
            VENDOR => Value::string("w3cos"),
            RENDERER => Value::string("w3cos wgpu compatibility renderer"),
            VERSION => Value::string(if self.webgl2 {
                "WebGL 2.0"
            } else {
                "WebGL 1.0"
            }),
            SHADING_LANGUAGE_VERSION => Value::string(if self.webgl2 {
                "WebGL GLSL ES 3.00"
            } else {
                "WebGL GLSL ES 1.00"
            }),
            MAX_VERTEX_ATTRIBS_PNAME => Value::Number(MAX_ATTRIBS as f64),
            MAX_TEXTURE_SIZE => Value::Number(4096.0),
            MAX_VIEWPORT_DIMS => Value::array(vec![Value::Number(4096.0), Value::Number(4096.0)]),
            ARRAY_BUFFER_BINDING => null_or(&self.array_buffer),
            ELEMENT_ARRAY_BUFFER_BINDING => null_or(&self.vao_mut().element),
            FRAMEBUFFER_BINDING => null_or(&self.draw_fbo),
            RENDERBUFFER_BINDING => null_or(&self.renderbuffer),
            TEXTURE_BINDING_2D => null_or(&self.textures_bound[self.active_unit]),
            ACTIVE_TEXTURE => Value::Number((TEXTURE0 + self.active_unit as u32) as f64),
            DEPTH_WRITEMASK => Value::Bool(self.depth_mask),
            DEPTH_FUNC => Value::Number(self.depth_func as f64),
            COLOR_WRITEMASK => {
                Value::array(self.color_mask.iter().copied().map(Value::Bool).collect())
            }
            BLEND_SRC_RGB => Value::Number(self.blend_src_rgb as f64),
            BLEND_DST_RGB => Value::Number(self.blend_dst_rgb as f64),
            BLEND_SRC_ALPHA => Value::Number(self.blend_src_a as f64),
            BLEND_DST_ALPHA => Value::Number(self.blend_dst_a as f64),
            BLEND_EQUATION => Value::Number(self.blend_eq_rgb as f64),
            BLEND_EQUATION_ALPHA => Value::Number(self.blend_eq_a as f64),
            CULL_FACE_MODE => Value::Number(self.cull_mode as f64),
            FRONT_FACE => Value::Number(if self.front_cw { CW } else { CCW } as f64),
            UNPACK_ALIGNMENT => Value::Number(self.unpack_alignment as f64),
            PACK_ALIGNMENT => Value::Number(self.pack_alignment as f64),
            UNPACK_FLIP_Y_WEBGL => Value::Bool(self.unpack_flip_y),
            UNPACK_PREMULTIPLY_ALPHA_WEBGL => Value::Bool(self.unpack_premultiply),
            VERTEX_ARRAY_BINDING => null_or(&self.vao),
            _ => Value::Null,
        }
    }

    fn clear(&mut self, mask: u32) {
        if mask & !(COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0 {
            self.record(INVALID_VALUE);
            return;
        }
        if mask & STENCIL_BUFFER_BIT != 0 {
            warn_once(
                "stencil-clear",
                "WebGL stencil clears are rejected; the wgpu drawing buffer has no stencil",
            );
        }
        if mask & (COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT) == 0 {
            return;
        }
        #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
        gpu::with_parked(self, |draw| {
            if let Err(error) = gpu::clear(draw, mask) {
                draw.record(error);
            }
        });
        #[cfg(not(all(feature = "gpu", feature = "web-graphics-advanced")))]
        self.no_gpu();
    }

    fn draw(
        &mut self,
        mode: u32,
        first: i32,
        count: i32,
        instances: i32,
        index: Option<(u32, u32)>,
    ) {
        if !matches!(
            mode,
            POINTS | LINES | LINE_LOOP | LINE_STRIP | TRIANGLES | TRIANGLE_STRIP | TRIANGLE_FAN
        ) {
            self.record(INVALID_ENUM);
            return;
        }
        if first < 0 || count < 0 || instances < 0 {
            self.record(INVALID_VALUE);
            return;
        }
        if count == 0 || instances == 0 {
            return;
        }
        if self.current_program().is_none() {
            self.record(INVALID_OPERATION);
            return;
        }
        if bound_id(&self.draw_fbo).is_some()
            && self.framebuffer_status(DRAW_FRAMEBUFFER) != FRAMEBUFFER_COMPLETE
        {
            self.record(INVALID_FRAMEBUFFER_OPERATION);
            return;
        }
        #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
        gpu::with_parked(self, |draw| {
            if let Err(error) = gpu::draw(
                draw,
                mode,
                first as u32,
                count as u32,
                instances as u32,
                index,
            ) {
                draw.record(error);
            }
        });
        #[cfg(not(all(feature = "gpu", feature = "web-graphics-advanced")))]
        {
            let _ = (mode, first, count, instances, index);
            self.no_gpu();
        }
    }

    fn read_pixels(&mut self, args: &[Value]) {
        let x = arg_i32(args, 0);
        let y = arg_i32(args, 1);
        let width = arg_i32(args, 2);
        let height = arg_i32(args, 3);
        let format = arg_u32(args, 4);
        let type_ = arg_u32(args, 5);
        let dest = args.get(6).cloned().unwrap_or(Value::Null);
        if width < 0 || height < 0 || x < 0 || y < 0 {
            self.record(INVALID_VALUE);
            return;
        }
        if format != RGBA || type_ != UNSIGNED_BYTE {
            warn_once(
                "read-pixels-format",
                "WebGL readPixels only executes RGBA UNSIGNED_BYTE on the wgpu compositor",
            );
            self.record(INVALID_OPERATION);
            return;
        }
        if !w3cos_core::binary::is_array_buffer_view(&dest) {
            self.record(INVALID_OPERATION);
            return;
        }
        #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
        gpu::with_parked(self, |draw| {
            if let Err(error) = gpu::read_pixels(draw, x, y, width, height, &dest) {
                draw.record(error);
            }
        });
        #[cfg(not(all(feature = "gpu", feature = "web-graphics-advanced")))]
        {
            let _ = (x, y, width, height, dest);
            self.no_gpu();
        }
    }
}

fn null_or(value: &Value) -> Value {
    if value.is_null() || value.is_undefined() {
        Value::Null
    } else {
        value.clone()
    }
}

fn is_sampler(gl_type: u32) -> bool {
    matches!(
        gl_type,
        SAMPLER_2D | 0x8B5F | 0x8B60 | 0x8B62 | 0x8DC1 | 0x8DC4 | 0x8DC5
    )
}

fn format_compatible(internal: u32, format: u32) -> bool {
    matches!(
        (internal, format),
        (RGBA | RGBA8, RGBA)
            | (RGB | RGB8, RGB)
            | (ALPHA, ALPHA)
            | (LUMINANCE, LUMINANCE)
            | (LUMINANCE_ALPHA, LUMINANCE_ALPHA)
    ) || internal == format
}

fn components_already_rgba(source: &Value) -> bool {
    source.get_property("__w3cos_rgba").to_bool()
}

fn bytes_value(pixels: &[u8]) -> Value {
    let values = pixels
        .iter()
        .map(|byte| Value::Number(*byte as f64))
        .collect();
    let array = w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Uint8Array"),
        vec![Value::array(values)],
    );
    array.set_property("__w3cos_rgba", Value::Bool(true));
    array
}

fn presented_canvas(node: u32) -> Option<(u32, u32, Vec<u8>)> {
    LIVE.with(|live| {
        for weak in live.borrow().iter() {
            let Some(state) = weak.upgrade() else {
                continue;
            };
            let state = state.borrow();
            if state.node_id == Some(node)
                && let Some(snapshot) = &state.snapshot
            {
                return Some((
                    snapshot.width,
                    snapshot.height,
                    snapshot.pixels.as_slice().to_vec(),
                ));
            }
        }
        None
    })
}

fn align_up(value: usize, alignment: usize) -> usize {
    value.div_ceil(alignment) * alignment
}

fn unpack_rgba(
    bytes: &[u8],
    width: u32,
    height: u32,
    components: usize,
    alignment: u32,
    row_length: u32,
    skip_rows: u32,
    skip_pixels: u32,
    alpha_only: bool,
) -> Result<Vec<u8>, u32> {
    let row_pixels = if row_length == 0 { width } else { row_length };
    let stride = align_up(row_pixels as usize * components, alignment.max(1) as usize);
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for row in 0..height as usize {
        let src_row = skip_rows as usize + row;
        for col in 0..width as usize {
            let src = src_row * stride + (skip_pixels as usize + col) * components;
            if src + components > bytes.len() {
                return Err(INVALID_OPERATION);
            }
            let dst = (row * width as usize + col) * 4;
            match components {
                4 => rgba[dst..dst + 4].copy_from_slice(&bytes[src..src + 4]),
                3 => {
                    rgba[dst..dst + 3].copy_from_slice(&bytes[src..src + 3]);
                    rgba[dst + 3] = 255;
                }
                2 => {
                    rgba[dst] = bytes[src];
                    rgba[dst + 1] = bytes[src];
                    rgba[dst + 2] = bytes[src];
                    rgba[dst + 3] = bytes[src + 1];
                }
                _ if alpha_only => {
                    rgba[dst + 3] = bytes[src];
                }
                _ => {
                    rgba[dst] = bytes[src];
                    rgba[dst + 1] = bytes[src];
                    rgba[dst + 2] = bytes[src];
                    rgba[dst + 3] = 255;
                }
            }
        }
    }
    Ok(rgba)
}

fn flip_rows(pixels: &mut [u8], width: u32, height: u32) {
    let stride = width as usize * 4;
    let height = height as usize;
    for row in 0..height / 2 {
        let other = height - 1 - row;
        let (left, right) = pixels.split_at_mut(other * stride);
        left[row * stride..row * stride + stride].swap_with_slice(&mut right[..stride]);
    }
}

fn premultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u16;
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u16 * alpha + 127) / 255) as u8;
        }
    }
}

fn blit_rgba(
    dest: &mut [u8],
    dest_width: u32,
    src: &[u8],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) {
    for row in 0..height as usize {
        let src_off = row * width as usize * 4;
        let dst_off = ((y as usize + row) * dest_width as usize + x as usize) * 4;
        let len = width as usize * 4;
        dest[dst_off..dst_off + len].copy_from_slice(&src[src_off..src_off + len]);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScalarKind {
    Float,
    Int,
    Uint,
}

struct UniformCall {
    matrix: bool,
    cols: usize,
    rows: usize,
    kind: ScalarKind,
    vector: bool,
}

fn parse_uniform_call(name: &str) -> Option<UniformCall> {
    let rest = name.strip_prefix("uniform")?;
    if let Some(rest) = rest.strip_prefix("Matrix") {
        let vector = rest.ends_with("fv");
        let body = rest.strip_suffix("fv")?;
        let (cols, rows) = if let Some((cols, rows)) = body.split_once('x') {
            (cols.parse().ok()?, rows.parse().ok()?)
        } else {
            let size = body.parse().ok()?;
            (size, size)
        };
        return Some(UniformCall {
            matrix: true,
            cols,
            rows,
            kind: ScalarKind::Float,
            vector,
        });
    }
    let kind = if rest.contains("ui") {
        ScalarKind::Uint
    } else if rest.contains('i') {
        ScalarKind::Int
    } else {
        ScalarKind::Float
    };
    let vector = rest.ends_with('v');
    let cols = rest.chars().find(|ch| ch.is_ascii_digit())?.to_digit(10)? as usize;
    Some(UniformCall {
        matrix: false,
        cols,
        rows: 1,
        kind,
        vector,
    })
}

fn uniform_element(name: &str) -> (String, u32) {
    if let Some((base, index)) = name
        .strip_suffix(']')
        .and_then(|rest| rest.rsplit_once('['))
        && let Ok(index) = index.parse()
    {
        return (base.to_string(), index);
    }
    (name.to_string(), 0)
}

fn matrix_shape(ty: &str) -> Option<(usize, usize)> {
    match ty {
        "mat2" | "mat2x2" => Some((2, 2)),
        "mat3" | "mat3x3" => Some((3, 3)),
        "mat4" | "mat4x4" => Some((4, 4)),
        "mat2x3" => Some((2, 3)),
        "mat2x4" => Some((2, 4)),
        "mat3x2" => Some((3, 2)),
        "mat3x4" => Some((3, 4)),
        "mat4x2" => Some((4, 2)),
        "mat4x3" => Some((4, 3)),
        _ => None,
    }
}

fn element_bytes(ty: &str) -> usize {
    if let Some((cols, _)) = matrix_shape(ty) {
        return cols * 16;
    }
    match ty {
        "float" | "int" | "uint" | "bool" => 4,
        "vec2" | "ivec2" | "uvec2" | "bvec2" => 8,
        "vec3" | "ivec3" | "uvec3" | "bvec3" => 12,
        _ => 16,
    }
}

fn array_stride(ty: &str) -> usize {
    if matrix_shape(ty).is_some() {
        element_bytes(ty)
    } else {
        16
    }
}

fn std140_bytes(variable: &Variable) -> usize {
    let count = variable.size.max(1) as usize;
    let bytes = if count > 1 {
        array_stride(&variable.glsl_type) * count
    } else {
        element_bytes(&variable.glsl_type)
    };
    bytes.max(16).next_multiple_of(16)
}

fn write_uniform(
    variable: &Variable,
    element: u32,
    call: &UniformCall,
    transpose: bool,
    values: &[f64],
    draw: &mut DrawState,
) -> Result<(), u32> {
    let program = draw.current_program().ok_or(INVALID_OPERATION)?;
    if let Some((cols, rows)) = matrix_shape(&variable.glsl_type) {
        if !call.matrix || call.cols != cols || call.rows != rows {
            return Err(INVALID_OPERATION);
        }
        let per = cols * rows;
        if per == 0 || values.len() % per != 0 {
            return Err(INVALID_VALUE);
        }
        let count = values.len() / per;
        let stored = draw.uniform_slot(program, variable);
        let stride = element_bytes(&variable.glsl_type);
        for matrix in 0..count {
            let dest = (element as usize + matrix) * stride;
            if dest + stride > stored.bytes.len() {
                return Err(INVALID_OPERATION);
            }
            for col in 0..cols {
                for row in 0..rows {
                    let src_index = matrix * per
                        + if transpose {
                            row * cols + col
                        } else {
                            col * rows + row
                        };
                    let value = values.get(src_index).copied().unwrap_or(0.0) as f32;
                    let at = dest + col * 16 + row * 4;
                    stored.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        return Ok(());
    }
    if call.matrix {
        return Err(INVALID_OPERATION);
    }
    let components = match variable.glsl_type.as_str() {
        "float" | "int" | "uint" | "bool" => 1,
        "vec2" | "ivec2" | "uvec2" | "bvec2" => 2,
        "vec3" | "ivec3" | "uvec3" | "bvec3" => 3,
        "vec4" | "ivec4" | "uvec4" | "bvec4" => 4,
        _ => return Err(INVALID_OPERATION),
    };
    if call.cols != components {
        return Err(INVALID_OPERATION);
    }
    if components == 0 || values.len() % components != 0 {
        return Err(INVALID_VALUE);
    }
    let count = values.len() / components;
    let stored = draw.uniform_slot(program, variable);
    let stride = if variable.size > 1 {
        array_stride(&variable.glsl_type)
    } else {
        element_bytes(&variable.glsl_type)
    };
    for item in 0..count {
        let dest = (element as usize + item) * stride;
        if dest + components * 4 > stored.bytes.len() && variable.size <= 1 && item > 0 {
            return Err(INVALID_OPERATION);
        }
        if dest + 4 > stored.bytes.len() {
            return Err(INVALID_OPERATION);
        }
        for component in 0..components {
            let value = values[item * components + component];
            let bytes = match call.kind {
                ScalarKind::Float => (value as f32).to_le_bytes(),
                ScalarKind::Int => (value as i32).to_le_bytes(),
                ScalarKind::Uint => (value as u32).to_le_bytes(),
            };
            let at = dest + component * 4;
            if at + 4 <= stored.bytes.len() {
                stored.bytes[at..at + 4].copy_from_slice(&bytes);
            }
        }
    }
    Ok(())
}

fn unpack_uniform(variable: &Variable, element: u32, bytes: &[u8]) -> Value {
    if let Some((cols, rows)) = matrix_shape(&variable.glsl_type) {
        let stride = element_bytes(&variable.glsl_type);
        let dest = element as usize * stride;
        let mut values = Vec::new();
        for col in 0..cols {
            for row in 0..rows {
                let at = dest + col * 16 + row * 4;
                let value = bytes
                    .get(at..at + 4)
                    .and_then(|chunk| chunk.try_into().ok())
                    .map(f32::from_le_bytes)
                    .unwrap_or(0.0);
                values.push(Value::Number(value as f64));
            }
        }
        return float_array(&values);
    }
    let components = match variable.glsl_type.as_str() {
        "float" | "int" | "uint" | "bool" => 1,
        "vec2" | "ivec2" | "uvec2" | "bvec2" => 2,
        "vec3" | "ivec3" | "uvec3" | "bvec3" => 3,
        _ => 4,
    };
    let stride = if variable.size > 1 {
        array_stride(&variable.glsl_type)
    } else {
        element_bytes(&variable.glsl_type)
    };
    let dest = element as usize * stride;
    let mut values = Vec::new();
    for component in 0..components {
        let at = dest + component * 4;
        let chunk = bytes
            .get(at..at + 4)
            .and_then(|chunk| chunk.try_into().ok());
        let number = if variable.glsl_type.starts_with('i') || variable.glsl_type == "int" {
            chunk.map(i32::from_le_bytes).unwrap_or(0) as f64
        } else if variable.glsl_type.starts_with('u') || variable.glsl_type == "uint" {
            chunk.map(u32::from_le_bytes).unwrap_or(0) as f64
        } else {
            chunk.map(f32::from_le_bytes).unwrap_or(0.0) as f64
        };
        values.push(Value::Number(number));
    }
    if components == 1 {
        values.pop().unwrap_or(Value::Number(0.0))
    } else {
        float_array(&values)
    }
}

fn float_array(values: &[Value]) -> Value {
    w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Float32Array"),
        vec![Value::array(values.to_vec())],
    )
}

fn read_numbers(value: &Value) -> Vec<f64> {
    if let Some(bytes) = w3cos_core::binary::bytes_of(value) {
        if w3cos_core::binary::typed_array_instance_of(
            value,
            &w3cos_core::binary::typed_array_class("Float32Array"),
        ) {
            return bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()) as f64)
                .collect();
        }
        if w3cos_core::binary::typed_array_instance_of(
            value,
            &w3cos_core::binary::typed_array_class("Int32Array"),
        ) {
            return bytes
                .chunks_exact(4)
                .map(|chunk| i32::from_le_bytes(chunk.try_into().unwrap()) as f64)
                .collect();
        }
        if w3cos_core::binary::typed_array_instance_of(
            value,
            &w3cos_core::binary::typed_array_class("Uint32Array"),
        ) {
            return bytes
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()) as f64)
                .collect();
        }
        if w3cos_core::binary::typed_array_instance_of(
            value,
            &w3cos_core::binary::typed_array_class("Float64Array"),
        ) {
            return bytes
                .chunks_exact(8)
                .map(|chunk| f64::from_le_bytes(chunk.try_into().unwrap()))
                .collect();
        }
    }
    let length = value.get_property("length").to_number();
    if value.is_object() && length.is_finite() && (0.0..1_000_000.0).contains(&length) {
        return (0..length as usize)
            .map(|index| value.get_property(&index.to_string()).to_number())
            .collect();
    }
    Vec::new()
}

#[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
mod gpu {
    use super::*;
    use std::sync::{Arc, Mutex, OnceLock};

    use std::ops::{Deref, DerefMut};

    use vello::wgpu;

    use crate::webgl_shader::{self, Stage};

    thread_local! {
        /// Owns wgpu resources between WebGL calls. Initialized only after this
        /// thread has taken a snatch lock, so it is destroyed while that lock's
        /// thread-local trace still exists.
        static GPU_PARK: RefCell<HashMap<u64, GpuResources>> = RefCell::new(HashMap::new());
        static PARK_DEPTH: Cell<u32> = const { Cell::new(0) };
    }

    struct SharedGpu {
        device: wgpu::Device,
        queue: wgpu::Queue,
        depth_format: wgpu::TextureFormat,
        fallback_view: wgpu::TextureView,
        fallback_sampler: wgpu::Sampler,
        clear_layout: wgpu::BindGroupLayout,
        clear_shader: wgpu::ShaderModule,
        clear_pipelines: Mutex<HashMap<(u8, bool, bool), wgpu::RenderPipeline>>,
        _fallback_texture: wgpu::Texture,
    }

    pub(super) struct GpuResources {
        color: Option<Target>,
        depth: Option<Target>,
        textures: HashMap<u64, GpuTexture>,
        modules: HashMap<u64, ProgramGpu>,
        pipelines: HashMap<PipeKey, wgpu::RenderPipeline>,
        uniforms: HashMap<(u64, String), wgpu::Buffer>,
        clear_uniform: Option<wgpu::Buffer>,
        depth_targets: HashMap<u64, Target>,
    }

    impl GpuResources {
        fn is_empty(&self) -> bool {
            self.color.is_none()
                && self.depth.is_none()
                && self.textures.is_empty()
                && self.modules.is_empty()
                && self.pipelines.is_empty()
                && self.uniforms.is_empty()
                && self.clear_uniform.is_none()
                && self.depth_targets.is_empty()
        }
    }

    impl Default for GpuResources {
        fn default() -> Self {
            Self {
                color: None,
                depth: None,
                textures: HashMap::new(),
                modules: HashMap::new(),
                pipelines: HashMap::new(),
                uniforms: HashMap::new(),
                clear_uniform: None,
                depth_targets: HashMap::new(),
            }
        }
    }

    pub(super) struct ContextGpu {
        inner: GpuResources,
    }

    impl Deref for ContextGpu {
        type Target = GpuResources;

        fn deref(&self) -> &GpuResources {
            &self.inner
        }
    }

    impl DerefMut for ContextGpu {
        fn deref_mut(&mut self) -> &mut GpuResources {
            &mut self.inner
        }
    }

    impl Drop for ContextGpu {
        fn drop(&mut self) {
            // Leftovers here are being dropped from the JS page arena, which
            // outlives wgpu's snatch trace. Leak them. Live resources are
            // parked and dropped from `GPU_PARK` or `release_parked`.
            std::mem::forget(std::mem::take(&mut self.inner));
        }
    }

    impl Default for ContextGpu {
        fn default() -> Self {
            Self {
                inner: GpuResources::default(),
            }
        }
    }

    pub(super) fn with_parked(draw: &mut DrawState, body: impl FnOnce(&mut DrawState)) {
        let depth = PARK_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current + 1);
            current
        });
        struct Guard<'a> {
            draw: &'a mut DrawState,
            depth: u32,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                if self.depth == 0 {
                    park(self.draw);
                }
                PARK_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
            }
        }
        if depth == 0
            && let Some(gpu) = shared()
        {
            prime_drop_order(&gpu.device);
        }
        let guard = Guard { draw, depth };
        if depth == 0 && shared().is_some() {
            unpark(guard.draw);
        }
        body(guard.draw);
    }

    pub(super) fn release_parked(ctx: u64) {
        let Some(resources) = GPU_PARK
            .try_with(|park| {
                park.try_borrow_mut()
                    .ok()
                    .and_then(|mut park| park.remove(&ctx))
            })
            .ok()
            .flatten()
        else {
            return;
        };
        drop(resources);
    }

    fn park(draw: &mut DrawState) {
        if draw.gpu.inner.is_empty() {
            return;
        }
        let ctx = draw.ctx;
        let resources = std::mem::take(&mut draw.gpu.inner);
        GPU_PARK.with(|park| park.borrow_mut().insert(ctx, resources));
    }

    fn unpark(draw: &mut DrawState) {
        let ctx = draw.ctx;
        if let Some(resources) = GPU_PARK.with(|park| park.borrow_mut().remove(&ctx)) {
            draw.gpu.inner = resources;
        }
    }

    fn prime_drop_order(device: &wgpu::Device) {
        thread_local! {
            static ONCE: Cell<bool> = const { Cell::new(false) };
        }
        if ONCE.with(|once| once.replace(true)) {
            return;
        }
        // Creating and dropping a buffer takes the snatch lock, which
        // initializes the trace this thread must still have when `GPU_PARK`
        // later drops real buffers.
        drop(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("webgl drop-order"),
            size: 4,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        drop(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("webgl drop-order"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
        GPU_PARK.with(|_| {});
    }

    struct Target {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        width: u32,
        height: u32,
    }

    struct GpuTexture {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        sampler: wgpu::Sampler,
        key: (u32, u32, u32, u32, u32, u32),
    }

    struct ProgramGpu {
        generation: u64,
        vertex: wgpu::ShaderModule,
        fragment: wgpu::ShaderModule,
        vertex_wgsl: String,
        fragment_wgsl: String,
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct PipeKey {
        program: u64,
        generation: u64,
        topology: u32,
        blend: bool,
        src_rgb: u32,
        dst_rgb: u32,
        src_a: u32,
        dst_a: u32,
        eq_rgb: u32,
        eq_a: u32,
        depth_test: bool,
        depth_write: bool,
        depth_func: u32,
        has_depth: bool,
        cull: u32,
        front_cw: bool,
        polygon_constant: i32,
        polygon_slope_bits: u32,
        mask: u8,
        attribs: Vec<(u32, u32, u32)>,
    }

    pub(super) fn finish() {
        if let Some(gpu) = shared() {
            gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        }
    }

    fn shared() -> Option<Arc<SharedGpu>> {
        static SLOT: OnceLock<Option<Arc<SharedGpu>>> = OnceLock::new();
        SLOT.get_or_init(open_adapter).clone()
    }

    pub(super) fn adapter_ready() -> bool {
        shared().is_some()
    }

    fn open_adapter() -> Option<Arc<SharedGpu>> {
        let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::all());
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
        });
        let adapter = pollster::block_on(async {
            if let Ok(adapter) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    force_fallback_adapter: true,
                    ..Default::default()
                })
                .await
            {
                return Some(adapter);
            }
            instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .ok()
        })?;
        let (device, queue) = request_device(&adapter, wgpu::Limits::default())
            .or_else(|| request_device(&adapter, wgpu::Limits::downlevel_defaults()))?;
        let depth_format = if adapter
            .get_texture_format_features(wgpu::TextureFormat::Depth24Plus)
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            wgpu::TextureFormat::Depth24Plus
        } else {
            wgpu::TextureFormat::Depth32Float
        };
        let fallback = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("webgl incomplete"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut pixel = vec![0u8; 256];
        pixel[3] = 255;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &fallback,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixel,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let fallback_view = fallback.create_view(&wgpu::TextureViewDescriptor::default());
        let fallback_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("webgl incomplete sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let clear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("webgl clear"),
            source: wgpu::ShaderSource::Wgsl(CLEAR_WGSL.into()),
        });
        let clear_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("webgl clear layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let info = adapter.get_info();
        eprintln!(
            "[w3cos] WebGL wgpu adapter: {} ({:?})",
            info.name, info.backend
        );
        Some(Arc::new(SharedGpu {
            device,
            queue,
            depth_format,
            fallback_view,
            fallback_sampler,
            clear_layout,
            clear_shader,
            clear_pipelines: Mutex::new(HashMap::new()),
            _fallback_texture: fallback,
        }))
    }

    fn request_device(
        adapter: &wgpu::Adapter,
        limits: wgpu::Limits,
    ) -> Option<(wgpu::Device, wgpu::Queue)> {
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("w3cos-webgl"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            ..Default::default()
        };
        pollster::block_on(adapter.request_device(&descriptor)).ok()
    }

    const CLEAR_WGSL: &str = r#"
struct ClearUniform {
    color: vec4<f32>,
    depth: f32,
}
@group(0) @binding(0) var<uniform> u: ClearUniform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(positions[index], u.depth, 1.0);
    return out;
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return u.color;
}
"#;

    struct View {
        width: u32,
        height: u32,
        has_depth: bool,
        default_fb: bool,
    }

    fn target_size(draw: &DrawState) -> Result<View, u32> {
        if let Some(id) = bound_id(&draw.draw_fbo) {
            let framebuffer = draw
                .framebuffers
                .get(&id)
                .ok_or(INVALID_FRAMEBUFFER_OPERATION)?;
            let color = framebuffer
                .color
                .and_then(|id| draw.textures.get(&id))
                .ok_or(INVALID_FRAMEBUFFER_OPERATION)?;
            if color.width == 0 || color.height == 0 {
                return Err(INVALID_FRAMEBUFFER_OPERATION);
            }
            return Ok(View {
                width: color.width,
                height: color.height,
                has_depth: framebuffer.depth.is_some(),
                default_fb: false,
            });
        }
        let (width, height) = draw.canvas_size();
        if width == 0 || height == 0 {
            return Err(NO_ERROR);
        }
        Ok(View {
            width,
            height,
            has_depth: true,
            default_fb: true,
        })
    }

    pub(super) fn clear(draw: &mut DrawState, mask: u32) -> Result<(), u32> {
        let Some(gpu) = shared() else {
            draw.no_gpu();
            return Ok(());
        };
        let view = target_size(draw)?;
        if view.width == 0 {
            return Ok(());
        }
        ensure_targets(draw, &gpu, &view)?;
        let color = mask & COLOR_BUFFER_BIT != 0;
        let depth = mask & DEPTH_BUFFER_BIT != 0 && view.has_depth && draw.depth_mask;
        if !color && !depth {
            return Ok(());
        }
        let full = scissor_covers(draw, view.width, view.height);
        let mask_all = draw.color_mask == [true, true, true, true];
        if full && (!color || mask_all) {
            load_clear(draw, &gpu, &view, color, depth)?;
        } else {
            triangle_clear(draw, &gpu, &view, color, depth)?;
        }
        if view.default_fb {
            draw.dirty = true;
        }
        Ok(())
    }

    fn load_clear(
        draw: &mut DrawState,
        gpu: &SharedGpu,
        view: &View,
        color: bool,
        depth: bool,
    ) -> Result<(), u32> {
        let color_view = color_view(draw, view)?;
        let depth_view = depth_view(draw, view);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("webgl clear"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("webgl clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: if color {
                            wgpu::LoadOp::Clear(wgpu_color(draw.clear_color))
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: depth_attachment(
                    depth_view.as_ref(),
                    depth,
                    draw.clear_depth,
                ),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn triangle_clear(
        draw: &mut DrawState,
        gpu: &SharedGpu,
        view: &View,
        color: bool,
        depth: bool,
    ) -> Result<(), u32> {
        let mask = if color { mask_bits(draw.color_mask) } else { 0 };
        if mask == 0 && !depth {
            return Ok(());
        }
        let pipeline = clear_pipeline(gpu, mask, view.has_depth, depth);
        let uniform = clear_uniform(draw, gpu);
        let bytes = clear_bytes(draw.clear_color, draw.clear_depth);
        gpu.queue.write_buffer(&uniform, 0, &bytes);
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("webgl clear group"),
            layout: &gpu.clear_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let color_view = color_view(draw, view)?;
        let depth_view = depth_view(draw, view);
        let scissor = scissor_rect(draw, view.width, view.height).ok_or(NO_ERROR)?;
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("webgl scissor clear"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("webgl scissor clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: depth_attachment(depth_view.as_ref(), false, 1.0),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.set_viewport(0.0, 0.0, view.width as f32, view.height as f32, 0.0, 1.0);
            pass.set_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn clear_bytes(color: [f32; 4], depth: f32) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for (index, channel) in color.iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&channel.to_le_bytes());
        }
        bytes[16..20].copy_from_slice(&depth.to_le_bytes());
        bytes
    }

    fn clear_uniform(draw: &mut DrawState, gpu: &SharedGpu) -> wgpu::Buffer {
        if let Some(buffer) = &draw.gpu.clear_uniform {
            return buffer.clone();
        }
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("webgl clear uniform"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        draw.gpu.clear_uniform = Some(buffer.clone());
        buffer
    }

    fn clear_pipeline(
        gpu: &SharedGpu,
        mask: u8,
        has_depth: bool,
        depth_write: bool,
    ) -> wgpu::RenderPipeline {
        let key = (mask, has_depth, depth_write);
        if let Some(pipeline) = gpu.clear_pipelines.lock().expect("clear").get(&key) {
            return pipeline.clone();
        }
        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("webgl clear pipeline layout"),
                bind_group_layouts: &[&gpu.clear_layout],
                immediate_size: 0,
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("webgl clear pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &gpu.clear_shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &gpu.clear_shader,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: color_writes(mask),
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: has_depth.then(|| wgpu::DepthStencilState {
                    format: gpu.depth_format,
                    depth_write_enabled: depth_write,
                    depth_compare: wgpu::CompareFunction::Always,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        gpu.clear_pipelines
            .lock()
            .expect("clear")
            .insert(key, pipeline.clone());
        pipeline
    }

    pub(super) fn draw(
        draw: &mut DrawState,
        mode: u32,
        first: u32,
        count: u32,
        instances: u32,
        index: Option<(u32, u32)>,
    ) -> Result<(), u32> {
        let Some(gpu) = shared() else {
            draw.no_gpu();
            return Ok(());
        };
        let view = target_size(draw)?;
        if view.width == 0 {
            return Ok(());
        }
        ensure_targets(draw, &gpu, &view)?;
        let program_id = draw.current_program().ok_or(INVALID_OPERATION)?;
        let linked = draw.linked(program_id).ok_or(INVALID_OPERATION)?;
        prepare_program(draw, &gpu, program_id, &linked)?;
        let inputs = vertex_inputs(&draw.gpu.modules[&program_id].vertex_wgsl);
        if inputs.is_empty() && !linked.attributes.is_empty() {
            eprintln!(
                "[w3cos] warning: WebGL vertex shader lowered with no entry inputs:\n{}",
                draw.gpu.modules[&program_id].vertex_wgsl
            );
            return Err(INVALID_OPERATION);
        }
        if draw.cull && draw.cull_mode == FRONT_AND_BACK && is_triangle(mode) {
            return Ok(());
        }
        let (topology, indices, vertex_start, vertex_count) =
            assemble(draw, mode, first, count, index)?;
        if vertex_count == 0 {
            return Ok(());
        }
        let mut attrib_data = Vec::new();
        let vao_attribs = if let Some(id) = bound_id(&draw.vao) {
            draw.vaos.get(&id).map(|vao| vao.attribs.clone())
        } else {
            Some(draw.default_vao.attribs.clone())
        }
        .ok_or(INVALID_OPERATION)?;
        for (location, components) in &inputs {
            let attrib = vao_attribs
                .get(*location as usize)
                .cloned()
                .unwrap_or_default();
            if attrib.enabled && attrib.divisor > 1 {
                warn_once(
                    "divisor",
                    "WebGL vertexAttribDivisor greater than 1 is rejected on the wgpu compositor",
                );
                return Err(INVALID_OPERATION);
            }
            let floats = if attrib.enabled {
                let buffer = attrib.buffer.ok_or(INVALID_OPERATION)?;
                let bytes = draw
                    .buffers
                    .get(&buffer)
                    .ok_or(INVALID_OPERATION)?
                    .bytes
                    .clone();
                let instances_or_vertices = if attrib.divisor == 0 {
                    vertex_count
                } else {
                    instances
                };
                let start = if attrib.divisor == 0 { vertex_start } else { 0 };
                expand_attrib(&attrib, &bytes, *components, start, instances_or_vertices)
            } else {
                expand_constant(attrib.current, *components, instances)
            };
            let step = if attrib.enabled && attrib.divisor == 0 {
                0
            } else {
                1
            };
            attrib_data.push((*location, *components, step, floats));
        }
        let key = pipe_key(
            draw,
            program_id,
            linked.generation,
            topology,
            view.has_depth,
            &attrib_data,
        );
        if !draw.gpu.pipelines.contains_key(&key) {
            let pipeline = build_pipeline(draw, &gpu, program_id, &key, view.has_depth)?;
            draw.gpu.pipelines.insert(key.clone(), pipeline);
        }
        let mut uploaded = Vec::new();
        for (_location, components, _step, floats) in &attrib_data {
            let bytes = f32_bytes(floats);
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("webgl attrib"),
                size: bytes.len().max(4) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !bytes.is_empty() {
                gpu.queue.write_buffer(&buffer, 0, &bytes);
            }
            uploaded.push((buffer, *components));
        }
        let index_buffer = if let Some(indices) = &indices {
            let bytes = indices
                .iter()
                .flat_map(|index| index.to_le_bytes())
                .collect::<Vec<_>>();
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("webgl index"),
                size: bytes.len().max(4) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue.write_buffer(&buffer, 0, &bytes);
            Some((buffer, indices.len() as u32))
        } else {
            None
        };
        let uniform_entries = write_uniforms(draw, &gpu, program_id, &linked)?;
        let sampler_entries = sampler_entries(draw, &gpu, &linked)?;
        let pipeline = draw.gpu.pipelines.get(&key).expect("pipeline").clone();
        let vertex_wgsl = draw.gpu.modules[&program_id].vertex_wgsl.clone();
        let fragment_wgsl = draw.gpu.modules[&program_id].fragment_wgsl.clone();
        let color_view = color_view(draw, &view)?;
        let depth_view = depth_view(draw, &view);
        let scissor = scissor_rect(draw, view.width, view.height);
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("webgl draw"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("webgl draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: depth_attachment(depth_view.as_ref(), false, 1.0),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            if has_group(&vertex_wgsl, 0) || has_group(&fragment_wgsl, 0) {
                let layout = pipeline.get_bind_group_layout(0);
                let entries = uniform_bind_entries(draw, program_id, &uniform_entries);
                let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("webgl uniforms"),
                    layout: &layout,
                    entries: &entries,
                });
                pass.set_bind_group(0, &bind, &[]);
            } else if has_group(&vertex_wgsl, 1) || has_group(&fragment_wgsl, 1) {
                // wgpu keeps bind group indices dense, so a sampler at group 1
                // still needs the empty group 0 that the auto layout inserts.
                let layout = pipeline.get_bind_group_layout(0);
                let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("webgl empty uniforms"),
                    layout: &layout,
                    entries: &[],
                });
                pass.set_bind_group(0, &bind, &[]);
            }
            if has_group(&fragment_wgsl, 1) || has_group(&vertex_wgsl, 1) {
                let layout = pipeline.get_bind_group_layout(1);
                let entries = sampler_bind_entries(&gpu, &sampler_entries, &draw.gpu.textures);
                let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("webgl samplers"),
                    layout: &layout,
                    entries: &entries,
                });
                pass.set_bind_group(1, &bind, &[]);
            }
            for (slot, (buffer, _)) in uploaded.iter().enumerate() {
                pass.set_vertex_buffer(slot as u32, buffer.slice(..));
            }
            let (vx, vy, vw, vh) = viewport_wgpu(draw, view.width, view.height);
            if vw > 0.0 && vh > 0.0 {
                pass.set_viewport(vx, vy, vw, vh, draw.depth_range[0], draw.depth_range[1]);
            }
            if let Some((x, y, w, h)) = scissor {
                pass.set_scissor_rect(x, y, w, h);
            }
            pass.set_blend_constant(wgpu_color(draw.blend_color));
            if let Some((buffer, index_count)) = &index_buffer {
                pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*index_count, 0, 0..instances);
            } else if vw > 0.0 && vh > 0.0 {
                pass.draw(0..vertex_count, 0..instances);
            }
        }
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        if let Some(error) = pollster::block_on(scope.pop()) {
            eprintln!("[w3cos] warning: WebGL draw was rejected by wgpu: {error}");
            return Err(INVALID_OPERATION);
        }
        if view.default_fb {
            draw.dirty = true;
        } else if let Some(id) = bound_id(&draw.draw_fbo)
            && let Some(color) = draw.framebuffers.get(&id).and_then(|fbo| fbo.color)
            && let Some(texture) = draw.textures.get_mut(&color)
        {
            texture.gpu_dirty = false;
        }
        Ok(())
    }

    fn prepare_program(
        draw: &mut DrawState,
        gpu: &SharedGpu,
        program: u64,
        linked: &LinkedGpu,
    ) -> Result<(), u32> {
        if draw
            .gpu
            .modules
            .get(&program)
            .is_some_and(|module| module.generation == linked.generation)
        {
            return Ok(());
        }
        let vertex_glsl = opengl_clip_z(&linked.vertex_glsl);
        let vertex_wgsl =
            webgl_shader::glsl_to_wgsl(Stage::Vertex, &vertex_glsl).map_err(|error| {
                warn_once(
                    "wgsl",
                    &format!("WebGL vertex shader did not lower to WGSL: {error}"),
                );
                INVALID_OPERATION
            })?;
        let fragment_wgsl = webgl_shader::glsl_to_wgsl(Stage::Fragment, &linked.fragment_glsl)
            .map_err(|error| {
                warn_once(
                    "wgsl-fragment",
                    &format!("WebGL fragment shader did not lower to WGSL: {error}"),
                );
                INVALID_OPERATION
            })?;
        let vertex = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("webgl vertex"),
                source: wgpu::ShaderSource::Wgsl(vertex_wgsl.clone().into()),
            });
        let fragment = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("webgl fragment"),
                source: wgpu::ShaderSource::Wgsl(fragment_wgsl.clone().into()),
            });
        draw.gpu.modules.insert(
            program,
            ProgramGpu {
                generation: linked.generation,
                vertex,
                fragment,
                vertex_wgsl,
                fragment_wgsl,
            },
        );
        draw.gpu.pipelines.retain(|key, _| key.program != program);
        Ok(())
    }

    fn opengl_clip_z(glsl: &str) -> String {
        if !glsl.contains("void main(") {
            return glsl.to_string();
        }
        let mut source = glsl.replacen("void main(", "void _w3cos_user_main(", 1);
        source.push_str(
            "\nvoid main() { _w3cos_user_main(); gl_Position.z = gl_Position.z * 0.5 + gl_Position.w * 0.5; }\n",
        );
        source
    }

    /// Vertex-shader inputs only. Output locations live on the result struct and
    /// must not become vertex buffers.
    fn vertex_inputs(wgsl: &str) -> Vec<(u32, usize)> {
        let Some(vertex_at) = wgsl.find("@vertex") else {
            return Vec::new();
        };
        let after = &wgsl[vertex_at..];
        let Some(fn_at) = after.find("fn ") else {
            return Vec::new();
        };
        let signature = &after[fn_at..];
        let Some(open) = signature.find('(') else {
            return Vec::new();
        };
        let Some(close) = matching_paren(&signature[open..]) else {
            return Vec::new();
        };
        let params = &signature[open + 1..open + close];
        if params.contains("@location") {
            return locations_in(params);
        }
        let type_name = params
            .rsplit(':')
            .next()
            .unwrap_or("")
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .find(|part| !part.is_empty())
            .unwrap_or("");
        if type_name.is_empty() {
            return Vec::new();
        }
        let marker = format!("struct {type_name}");
        let Some(start) = wgsl.find(&marker) else {
            return Vec::new();
        };
        let body = &wgsl[start..];
        let Some(brace) = body.find('{') else {
            return Vec::new();
        };
        let Some(end) = body[brace..].find('}') else {
            return Vec::new();
        };
        locations_in(&body[brace + 1..brace + end])
    }

    fn matching_paren(source: &str) -> Option<usize> {
        let mut depth = 0i32;
        for (index, ch) in source.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn locations_in(source: &str) -> Vec<(u32, usize)> {
        let mut found = Vec::new();
        let mut rest = source;
        while let Some(index) = rest.find("@location(") {
            let after = &rest[index + "@location(".len()..];
            let digits: String = after.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if let Ok(location) = digits.parse::<u32>() {
                let window = &after[..after.len().min(80)];
                let components = if window.contains("vec4") {
                    4
                } else if window.contains("vec3") {
                    3
                } else if window.contains("vec2") {
                    2
                } else {
                    1
                };
                found.push((location, components));
            }
            rest = &after[digits.len()..];
        }
        found.sort_by_key(|(location, _)| *location);
        found.dedup_by_key(|(location, _)| *location);
        found
    }

    fn has_group(wgsl: &str, group: u32) -> bool {
        wgsl.contains(&format!("@group({group})")) || wgsl.contains(&format!("@group({group}u)"))
    }

    fn expand_constant(current: [f32; 4], components: usize, count: u32) -> Vec<f32> {
        let mut out = Vec::with_capacity(count as usize * components);
        for _ in 0..count {
            for component in 0..components {
                out.push(current.get(component).copied().unwrap_or(0.0));
            }
        }
        out
    }

    fn expand_attrib(
        attrib: &Attrib,
        bytes: &[u8],
        components: usize,
        start: u32,
        count: u32,
    ) -> Vec<f32> {
        let type_size = gl_type_size(attrib.type_);
        let stride = if attrib.stride == 0 {
            attrib.size.saturating_mul(type_size)
        } else {
            attrib.stride
        };
        let mut out = Vec::with_capacity(count as usize * components);
        for vertex in 0..count {
            let base = attrib.offset as usize + (start + vertex) as usize * stride as usize;
            for component in 0..components {
                let value = if (component as u32) < attrib.size {
                    read_component(bytes, base, attrib, component)
                } else if component == 3 {
                    1.0
                } else {
                    0.0
                };
                out.push(value);
            }
        }
        out
    }

    fn gl_type_size(type_: u32) -> u32 {
        match type_ {
            BYTE | UNSIGNED_BYTE => 1,
            SHORT | UNSIGNED_SHORT => 2,
            _ => 4,
        }
    }

    fn read_component(bytes: &[u8], base: usize, attrib: &Attrib, component: usize) -> f32 {
        let size = gl_type_size(attrib.type_) as usize;
        let offset = base + component * size;
        if offset + size > bytes.len() {
            return if component == 3 { 1.0 } else { 0.0 };
        }
        match attrib.type_ {
            FLOAT => f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()),
            UNSIGNED_BYTE => {
                let value = bytes[offset] as f32;
                if attrib.normalized {
                    value / 255.0
                } else {
                    value
                }
            }
            BYTE => {
                let value = bytes[offset] as i8 as f32;
                if attrib.normalized {
                    (value / 127.0).max(-1.0)
                } else {
                    value
                }
            }
            UNSIGNED_SHORT => {
                let value =
                    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as f32;
                if attrib.normalized {
                    value / 65535.0
                } else {
                    value
                }
            }
            SHORT => {
                let value =
                    i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as f32;
                if attrib.normalized {
                    (value / 32767.0).max(-1.0)
                } else {
                    value
                }
            }
            UNSIGNED_INT => {
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as f32
            }
            INT => i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as f32,
            _ => 0.0,
        }
    }

    fn assemble(
        draw: &DrawState,
        mode: u32,
        first: u32,
        count: u32,
        index: Option<(u32, u32)>,
    ) -> Result<(u32, Option<Vec<u32>>, u32, u32), u32> {
        let Some((type_, offset)) = index else {
            return match mode {
                LINE_LOOP => Ok((LINES, Some(line_loop(count)), first, count)),
                TRIANGLE_FAN => Ok((
                    TRIANGLES,
                    Some(fan(&(0..count).collect::<Vec<_>>())),
                    first,
                    count,
                )),
                _ => Ok((mode, None, first, count)),
            };
        };
        // Indexed line loop uses the line-list topology; fan and strips become lists.
        let element = if let Some(id) = bound_id(&draw.vao) {
            let vao = draw.vaos.get(&id).ok_or(INVALID_OPERATION)?;
            bound_id(&vao.element).ok_or(INVALID_OPERATION)?
        } else {
            bound_id(&draw.default_vao.element).ok_or(INVALID_OPERATION)?
        };
        let bytes = draw
            .buffers
            .get(&element)
            .ok_or(INVALID_OPERATION)?
            .bytes
            .clone();
        let indices = read_indices(&bytes, type_, offset, count)?;
        let vertex_count = indices.iter().copied().max().unwrap_or(0).saturating_add(1);
        if vertex_count > 1_048_576 {
            return Err(INVALID_OPERATION);
        }
        let (topology, indices) = match mode {
            TRIANGLES | LINES | POINTS => (mode, indices),
            LINE_STRIP => (LINES, line_strip(&indices)),
            LINE_LOOP => (LINES, line_loop_indexed(&indices)),
            TRIANGLE_STRIP => (TRIANGLES, triangle_strip(&indices)),
            TRIANGLE_FAN => (TRIANGLES, fan(&indices)),
            _ => return Err(INVALID_ENUM),
        };
        Ok((topology, Some(indices), 0, vertex_count))
    }

    fn read_indices(bytes: &[u8], type_: u32, offset: u32, count: u32) -> Result<Vec<u32>, u32> {
        let size = match type_ {
            UNSIGNED_BYTE => 1,
            UNSIGNED_SHORT => 2,
            UNSIGNED_INT => 4,
            _ => return Err(INVALID_ENUM),
        };
        let start = offset as usize;
        if start + count as usize * size > bytes.len() {
            return Err(INVALID_OPERATION);
        }
        let mut indices = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            let at = start + index * size;
            indices.push(match size {
                1 => bytes[at] as u32,
                2 => u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()) as u32,
                _ => u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()),
            });
        }
        Ok(indices)
    }

    fn fan(indices: &[u32]) -> Vec<u32> {
        let mut out = Vec::new();
        if indices.len() < 3 {
            return out;
        }
        for index in 1..indices.len() - 1 {
            out.extend([indices[0], indices[index], indices[index + 1]]);
        }
        out
    }

    fn triangle_strip(indices: &[u32]) -> Vec<u32> {
        let mut out = Vec::new();
        if indices.len() < 3 {
            return out;
        }
        for index in 0..indices.len() - 2 {
            if index % 2 == 0 {
                out.extend([indices[index], indices[index + 1], indices[index + 2]]);
            } else {
                out.extend([indices[index + 1], indices[index], indices[index + 2]]);
            }
        }
        out
    }

    fn line_strip(indices: &[u32]) -> Vec<u32> {
        indices
            .windows(2)
            .flat_map(|pair| [pair[0], pair[1]])
            .collect()
    }

    fn line_loop(count: u32) -> Vec<u32> {
        if count < 2 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for index in 0..count - 1 {
            out.extend([index, index + 1]);
        }
        out.extend([count - 1, 0]);
        out
    }

    fn line_loop_indexed(indices: &[u32]) -> Vec<u32> {
        if indices.len() < 2 {
            return Vec::new();
        }
        let mut out = line_strip(indices);
        out.extend([indices[indices.len() - 1], indices[0]]);
        out
    }

    fn is_triangle(mode: u32) -> bool {
        matches!(mode, TRIANGLES | TRIANGLE_STRIP | TRIANGLE_FAN)
    }

    fn pipe_key(
        draw: &DrawState,
        program: u64,
        generation: u64,
        topology: u32,
        has_depth: bool,
        attribs: &[(u32, usize, u32, Vec<f32>)],
    ) -> PipeKey {
        PipeKey {
            program,
            generation,
            topology,
            blend: draw.blend,
            src_rgb: draw.blend_src_rgb,
            dst_rgb: draw.blend_dst_rgb,
            src_a: draw.blend_src_a,
            dst_a: draw.blend_dst_a,
            eq_rgb: draw.blend_eq_rgb,
            eq_a: draw.blend_eq_a,
            depth_test: draw.depth_test,
            depth_write: draw.depth_mask,
            depth_func: draw.depth_func,
            has_depth,
            cull: if draw.cull { draw.cull_mode } else { 0 },
            front_cw: draw.front_cw,
            polygon_constant: if draw.polygon_offset && draw.depth_test {
                draw.polygon_units as i32
            } else {
                0
            },
            polygon_slope_bits: if draw.polygon_offset && draw.depth_test {
                draw.polygon_factor.to_bits()
            } else {
                0
            },
            mask: mask_bits(draw.color_mask),
            attribs: attribs
                .iter()
                .map(|(location, components, step, _)| (*location, *components as u32, *step))
                .collect(),
        }
    }

    fn build_pipeline(
        draw: &DrawState,
        gpu: &SharedGpu,
        program: u64,
        key: &PipeKey,
        has_depth: bool,
    ) -> Result<wgpu::RenderPipeline, u32> {
        let module = draw.gpu.modules.get(&program).ok_or(INVALID_OPERATION)?;
        // VertexBufferLayout borrows attributes, so keep them beside the layouts.
        let attributes = key
            .attribs
            .iter()
            .map(|(location, components, _)| {
                let format = match components {
                    1 => wgpu::VertexFormat::Float32,
                    2 => wgpu::VertexFormat::Float32x2,
                    3 => wgpu::VertexFormat::Float32x3,
                    _ => wgpu::VertexFormat::Float32x4,
                };
                vec![wgpu::VertexAttribute {
                    format,
                    offset: 0,
                    shader_location: *location,
                }]
            })
            .collect::<Vec<_>>();
        let layouts = attributes
            .iter()
            .enumerate()
            .map(|(index, attributes)| wgpu::VertexBufferLayout {
                array_stride: (key.attribs[index].1 as u64) * 4,
                step_mode: if key.attribs[index].2 == 0 {
                    wgpu::VertexStepMode::Vertex
                } else {
                    wgpu::VertexStepMode::Instance
                },
                attributes,
            })
            .collect::<Vec<_>>();
        let descriptor_buffers = layouts;
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("webgl"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module.vertex,
                    entry_point: Some("main"),
                    buffers: &descriptor_buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module.fragment,
                    entry_point: Some("main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: Some(if key.blend {
                            wgpu::BlendState {
                                color: blend_component(key.src_rgb, key.dst_rgb, key.eq_rgb),
                                alpha: blend_component(key.src_a, key.dst_a, key.eq_a),
                            }
                        } else {
                            wgpu::BlendState::REPLACE
                        }),
                        write_mask: color_writes(key.mask),
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: topology(key.topology),
                    front_face: if key.front_cw {
                        wgpu::FrontFace::Cw
                    } else {
                        wgpu::FrontFace::Ccw
                    },
                    cull_mode: cull_mode(key.cull),
                    ..Default::default()
                },
                depth_stencil: has_depth.then(|| wgpu::DepthStencilState {
                    format: gpu.depth_format,
                    depth_write_enabled: key.depth_write,
                    depth_compare: if key.depth_test {
                        compare(key.depth_func)
                    } else {
                        wgpu::CompareFunction::Always
                    },
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState {
                        constant: key.polygon_constant,
                        slope_scale: f32::from_bits(key.polygon_slope_bits),
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        if let Some(error) = pollster::block_on(scope.pop()) {
            eprintln!(
                "[w3cos] warning: WebGL pipeline was rejected by wgpu: {error}\nvertex:\n{}\nfragment:\n{}",
                module.vertex_wgsl, module.fragment_wgsl
            );
            return Err(INVALID_OPERATION);
        }
        Ok(pipeline)
    }

    fn topology(mode: u32) -> wgpu::PrimitiveTopology {
        match mode {
            POINTS => wgpu::PrimitiveTopology::PointList,
            LINES => wgpu::PrimitiveTopology::LineList,
            LINE_STRIP => wgpu::PrimitiveTopology::LineStrip,
            TRIANGLE_STRIP => wgpu::PrimitiveTopology::TriangleStrip,
            _ => wgpu::PrimitiveTopology::TriangleList,
        }
    }

    fn cull_mode(mode: u32) -> Option<wgpu::Face> {
        match mode {
            FRONT => Some(wgpu::Face::Front),
            BACK => Some(wgpu::Face::Back),
            _ => None,
        }
    }

    fn compare(func: u32) -> wgpu::CompareFunction {
        match func {
            NEVER => wgpu::CompareFunction::Never,
            LESS => wgpu::CompareFunction::Less,
            EQUAL => wgpu::CompareFunction::Equal,
            LEQUAL => wgpu::CompareFunction::LessEqual,
            GREATER => wgpu::CompareFunction::Greater,
            NOTEQUAL => wgpu::CompareFunction::NotEqual,
            GEQUAL => wgpu::CompareFunction::GreaterEqual,
            _ => wgpu::CompareFunction::Always,
        }
    }

    fn blend_component(src: u32, dst: u32, equation: u32) -> wgpu::BlendComponent {
        wgpu::BlendComponent {
            src_factor: blend_factor(src),
            dst_factor: blend_factor(dst),
            operation: blend_operation(equation),
        }
    }

    fn blend_factor(factor: u32) -> wgpu::BlendFactor {
        match factor {
            ONE => wgpu::BlendFactor::One,
            SRC_COLOR => wgpu::BlendFactor::Src,
            ONE_MINUS_SRC_COLOR => wgpu::BlendFactor::OneMinusSrc,
            SRC_ALPHA => wgpu::BlendFactor::SrcAlpha,
            ONE_MINUS_SRC_ALPHA => wgpu::BlendFactor::OneMinusSrcAlpha,
            DST_ALPHA => wgpu::BlendFactor::DstAlpha,
            ONE_MINUS_DST_ALPHA => wgpu::BlendFactor::OneMinusDstAlpha,
            DST_COLOR => wgpu::BlendFactor::Dst,
            ONE_MINUS_DST_COLOR => wgpu::BlendFactor::OneMinusDst,
            SRC_ALPHA_SATURATE => wgpu::BlendFactor::SrcAlphaSaturated,
            CONSTANT_COLOR | CONSTANT_ALPHA => wgpu::BlendFactor::Constant,
            ONE_MINUS_CONSTANT_COLOR | ONE_MINUS_CONSTANT_ALPHA => {
                wgpu::BlendFactor::OneMinusConstant
            }
            _ => wgpu::BlendFactor::Zero,
        }
    }

    fn blend_operation(equation: u32) -> wgpu::BlendOperation {
        match equation {
            FUNC_SUBTRACT => wgpu::BlendOperation::Subtract,
            FUNC_REVERSE_SUBTRACT => wgpu::BlendOperation::ReverseSubtract,
            MIN => wgpu::BlendOperation::Min,
            MAX => wgpu::BlendOperation::Max,
            _ => wgpu::BlendOperation::Add,
        }
    }

    fn write_uniforms(
        draw: &mut DrawState,
        gpu: &SharedGpu,
        program: u64,
        linked: &LinkedGpu,
    ) -> Result<Vec<(u32, String)>, u32> {
        let wgsl = format!(
            "{}{}",
            draw.gpu.modules[&program].vertex_wgsl, draw.gpu.modules[&program].fragment_wgsl
        );
        let mut entries = Vec::new();
        for (index, uniform) in linked.uniforms.iter().enumerate() {
            if is_sampler(uniform.gl_type) {
                continue;
            }
            if !group_binding(&wgsl, 0, index as u32) {
                continue;
            }
            let bytes = draw
                .uniforms
                .get(&(program, uniform.name.clone()))
                .map(|stored| stored.bytes.clone())
                .unwrap_or_else(|| vec![0u8; std140_bytes(uniform)]);
            let size = (bytes.len().max(16) as u64).next_multiple_of(16);
            let key = (program, uniform.name.clone());
            let buffer = if let Some(buffer) = draw.gpu.uniforms.get(&key)
                && buffer.size() >= size
            {
                buffer.clone()
            } else {
                let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("webgl uniform"),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                draw.gpu.uniforms.insert(key.clone(), buffer.clone());
                buffer
            };
            gpu.queue.write_buffer(&buffer, 0, &bytes);
            entries.push((index as u32, uniform.name.clone()));
        }
        Ok(entries)
    }

    struct SamplerBind {
        binding_texture: u32,
        binding_sampler: u32,
        view_id: Option<u64>,
        filter: (u32, u32, u32, u32),
    }

    fn sampler_entries(
        draw: &mut DrawState,
        gpu: &SharedGpu,
        linked: &LinkedGpu,
    ) -> Result<Vec<SamplerBind>, u32> {
        let program = draw.current_program().ok_or(INVALID_OPERATION)?;
        let module = &draw.gpu.modules[&program];
        // `String + &String` fails to resolve once smartstring is in the
        // crate graph (default features). Build the scan buffer explicitly.
        let wgsl = format!("{}{}", module.fragment_wgsl, module.vertex_wgsl);
        let mut entries = Vec::new();
        for (index, uniform) in linked.uniforms.iter().enumerate() {
            if uniform.gl_type != SAMPLER_2D {
                if is_sampler(uniform.gl_type) && group_binding(&wgsl, 1, (index as u32) * 2) {
                    warn_once(
                        "sampler",
                        "WebGL sampler uniforms other than sampler2D are rejected on the wgpu compositor",
                    );
                    return Err(INVALID_OPERATION);
                }
                continue;
            }
            let texture_binding = (index as u32) * 2;
            let sampler_binding = texture_binding + 1;
            if !group_binding(&wgsl, 1, texture_binding) {
                continue;
            }
            let unit = draw
                .uniforms
                .get(&(program, uniform.name.clone()))
                .and_then(|stored| stored.sampler_units.first().copied())
                .unwrap_or(0) as usize;
            let texture_id = draw
                .textures_bound
                .get(unit)
                .and_then(bound_id)
                .filter(|id| {
                    draw.textures.get(id).is_some_and(|texture| {
                        texture.width > 0
                            && texture.height > 0
                            && (texture.mips_requested
                                || matches!(texture.min_filter, NEAREST | LINEAR))
                    })
                });
            if let Some(id) = texture_id {
                upload_texture(draw, gpu, id)?;
            } else if draw
                .textures
                .get(
                    &draw
                        .textures_bound
                        .get(unit)
                        .and_then(bound_id)
                        .unwrap_or(0),
                )
                .is_some_and(|texture| {
                    !matches!(texture.min_filter, NEAREST | LINEAR) && !texture.mips_requested
                })
            {
                warn_once(
                    "incomplete-texture",
                    "WebGL sampled an incomplete mipmapped texture; the sample is (0,0,0,1)",
                );
            }
            let filter = texture_id
                .and_then(|id| draw.textures.get(&id))
                .map(|texture| {
                    (
                        texture.min_filter,
                        texture.mag_filter,
                        texture.wrap_s,
                        texture.wrap_t,
                    )
                })
                .unwrap_or((NEAREST, NEAREST, CLAMP_TO_EDGE, CLAMP_TO_EDGE));
            entries.push(SamplerBind {
                binding_texture: texture_binding,
                binding_sampler: sampler_binding,
                view_id: texture_id,
                filter,
            });
        }
        Ok(entries)
    }

    fn upload_texture(draw: &mut DrawState, gpu: &SharedGpu, id: u64) -> Result<(), u32> {
        let texture = draw.textures.get(&id).ok_or(INVALID_OPERATION)?;
        let key = (
            texture.width,
            texture.height,
            texture.min_filter,
            texture.mag_filter,
            texture.wrap_s,
            texture.wrap_t,
        );
        let dirty = texture.gpu_dirty
            || draw
                .gpu
                .textures
                .get(&id)
                .is_none_or(|gpu_texture| gpu_texture.key != key);
        if !dirty {
            return Ok(());
        }
        if matches!(
            texture.min_filter,
            NEAREST_MIPMAP_NEAREST
                | LINEAR_MIPMAP_NEAREST
                | NEAREST_MIPMAP_LINEAR
                | LINEAR_MIPMAP_LINEAR
        ) {
            warn_once(
                "mip-sample",
                "WebGL mipmapped sampling uses level 0; mipmap chains are not built",
            );
        }
        let pixels = texture.pixels.clone();
        let (width, height) = (texture.width.max(1), texture.height.max(1));
        let gpu_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("webgl texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let padded = padded_bytes(width);
        let mut upload = vec![0u8; padded as usize * height as usize];
        for row in 0..height as usize {
            let src = row * width as usize * 4;
            let dst = row * padded as usize;
            let len = width as usize * 4;
            if src + len <= pixels.len() {
                upload[dst..dst + len].copy_from_slice(&pixels[src..src + len]);
            }
        }
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &upload,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = gpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = gpu
            .device
            .create_sampler(&sampler_desc(key.2, key.3, key.4, key.5));
        draw.gpu.textures.insert(
            id,
            GpuTexture {
                texture: gpu_texture,
                view,
                sampler,
                key,
            },
        );
        if let Some(texture) = draw.textures.get_mut(&id) {
            texture.gpu_dirty = false;
        }
        Ok(())
    }

    fn sampler_desc(
        min: u32,
        mag: u32,
        wrap_s: u32,
        wrap_t: u32,
    ) -> wgpu::SamplerDescriptor<'static> {
        wgpu::SamplerDescriptor {
            label: Some("webgl sampler"),
            address_mode_u: address(wrap_s),
            address_mode_v: address(wrap_t),
            mag_filter: if mag == LINEAR {
                wgpu::FilterMode::Linear
            } else {
                wgpu::FilterMode::Nearest
            },
            min_filter: if matches!(min, LINEAR | LINEAR_MIPMAP_NEAREST | LINEAR_MIPMAP_LINEAR) {
                wgpu::FilterMode::Linear
            } else {
                wgpu::FilterMode::Nearest
            },
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        }
    }

    fn address(mode: u32) -> wgpu::AddressMode {
        match mode {
            CLAMP_TO_EDGE => wgpu::AddressMode::ClampToEdge,
            MIRRORED_REPEAT => wgpu::AddressMode::MirrorRepeat,
            _ => wgpu::AddressMode::Repeat,
        }
    }

    fn uniform_bind_entries<'a>(
        draw: &'a DrawState,
        program: u64,
        entries: &'a [(u32, String)],
    ) -> Vec<wgpu::BindGroupEntry<'a>> {
        entries
            .iter()
            .filter_map(|(binding, name)| {
                let buffer = draw.gpu.uniforms.get(&(program, name.clone()))?;
                Some(wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: buffer.as_entire_binding(),
                })
            })
            .collect()
    }

    fn sampler_bind_entries<'a>(
        gpu: &'a SharedGpu,
        entries: &'a [SamplerBind],
        draw_views: &'a HashMap<u64, GpuTexture>,
    ) -> Vec<wgpu::BindGroupEntry<'a>> {
        let mut out = Vec::new();
        for entry in entries {
            let (view, sampler) = if let Some(id) = entry.view_id.and_then(|id| draw_views.get(&id))
            {
                (&id.view, &id.sampler)
            } else {
                (&gpu.fallback_view, &gpu.fallback_sampler)
            };
            let _ = entry.filter;
            out.push(wgpu::BindGroupEntry {
                binding: entry.binding_texture,
                resource: wgpu::BindingResource::TextureView(view),
            });
            out.push(wgpu::BindGroupEntry {
                binding: entry.binding_sampler,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        out
    }

    fn group_binding(wgsl: &str, group: u32, binding: u32) -> bool {
        for group_pat in [format!("@group({group})"), format!("@group({group}u)")] {
            let mut rest = wgsl;
            while let Some(index) = rest.find(&group_pat) {
                let start = index.saturating_sub(48);
                let end = (index + group_pat.len() + 48).min(rest.len());
                let window = &rest[start..end];
                if window.contains(&format!("@binding({binding})"))
                    || window.contains(&format!("@binding({binding}u)"))
                {
                    return true;
                }
                rest = &rest[index + group_pat.len()..];
            }
        }
        false
    }

    fn f32_bytes(values: &[f32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn ensure_targets(draw: &mut DrawState, gpu: &SharedGpu, view: &View) -> Result<(), u32> {
        if view.default_fb {
            let replace =
                draw.gpu.color.as_ref().is_none_or(|target| {
                    target.width != view.width || target.height != view.height
                });
            if replace {
                draw.gpu.color = Some(color_target(&gpu.device, view.width, view.height));
                draw.gpu.depth = Some(depth_target(
                    &gpu.device,
                    gpu.depth_format,
                    view.width,
                    view.height,
                ));
                draw.dirty = true;
                load_clear(draw, gpu, view, true, true)?;
            }
            return Ok(());
        }
        let fbo = bound_id(&draw.draw_fbo).ok_or(INVALID_FRAMEBUFFER_OPERATION)?;
        let color_id = draw
            .framebuffers
            .get(&fbo)
            .and_then(|framebuffer| framebuffer.color)
            .ok_or(INVALID_FRAMEBUFFER_OPERATION)?;
        upload_texture(draw, gpu, color_id)?;
        if view.has_depth {
            let depth_id = draw.framebuffers[&fbo].depth.unwrap_or(0);
            let size_ok =
                draw.gpu.depth_targets.get(&depth_id).is_some_and(|target| {
                    target.width == view.width && target.height == view.height
                });
            if !size_ok {
                draw.gpu.depth_targets.insert(
                    depth_id,
                    depth_target(&gpu.device, gpu.depth_format, view.width, view.height),
                );
            }
        }
        Ok(())
    }

    fn color_target(device: &wgpu::Device, width: u32, height: u32) -> Target {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("webgl color"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Target {
            texture,
            view,
            width,
            height,
        }
    }

    fn depth_target(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Target {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("webgl depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Target {
            texture,
            view,
            width,
            height,
        }
    }

    fn color_view(draw: &DrawState, view: &View) -> Result<wgpu::TextureView, u32> {
        if view.default_fb {
            return Ok(draw
                .gpu
                .color
                .as_ref()
                .ok_or(INVALID_OPERATION)?
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()));
        }
        let fbo = bound_id(&draw.draw_fbo).ok_or(INVALID_FRAMEBUFFER_OPERATION)?;
        let color = draw.framebuffers[&fbo]
            .color
            .ok_or(INVALID_FRAMEBUFFER_OPERATION)?;
        Ok(draw
            .gpu
            .textures
            .get(&color)
            .ok_or(INVALID_FRAMEBUFFER_OPERATION)?
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default()))
    }

    fn depth_view(draw: &DrawState, view: &View) -> Option<wgpu::TextureView> {
        if !view.has_depth {
            return None;
        }
        if view.default_fb {
            return draw.gpu.depth.as_ref().map(|target| {
                target
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default())
            });
        }
        let fbo = bound_id(&draw.draw_fbo)?;
        let depth = draw.framebuffers.get(&fbo)?.depth?;
        draw.gpu.depth_targets.get(&depth).map(|target| {
            target
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default())
        })
    }

    fn depth_attachment(
        view: Option<&wgpu::TextureView>,
        clear: bool,
        depth: f32,
    ) -> Option<wgpu::RenderPassDepthStencilAttachment<'_>> {
        view.map(|view| wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: Some(wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(depth)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        })
    }

    fn scissor_rect(draw: &DrawState, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        if !draw.scissor_on {
            return Some((0, 0, width, height));
        }
        intersect_bottom_left(draw.scissor, width, height)
    }

    fn scissor_covers(draw: &DrawState, width: u32, height: u32) -> bool {
        matches!(scissor_rect(draw, width, height), Some((0, 0, w, h)) if w == width && h == height)
    }

    fn viewport_wgpu(draw: &DrawState, _width: u32, height: u32) -> (f32, f32, f32, f32) {
        let [x, y, w, h] = draw.viewport;
        let wgpu_y = height as f32 - y as f32 - h as f32;
        (x as f32, wgpu_y, w as f32, h as f32)
    }

    fn intersect_bottom_left(
        rect: [i32; 4],
        width: u32,
        height: u32,
    ) -> Option<(u32, u32, u32, u32)> {
        let [x, y, w, h] = rect;
        if w <= 0 || h <= 0 {
            return None;
        }
        let x1 = x.saturating_add(w);
        let y1 = y.saturating_add(h);
        let top = height as i32 - y1;
        let bottom = height as i32 - y;
        let left = x.max(0);
        let right = x1.min(width as i32);
        let top = top.max(0);
        let bottom = bottom.min(height as i32);
        if right <= left || bottom <= top {
            return None;
        }
        Some((
            left as u32,
            top as u32,
            (right - left) as u32,
            (bottom - top) as u32,
        ))
    }

    fn mask_bits(mask: [bool; 4]) -> u8 {
        mask.iter()
            .enumerate()
            .fold(0, |bits, (index, on)| bits | ((*on as u8) << index))
    }

    fn color_writes(mask: u8) -> wgpu::ColorWrites {
        let mut writes = wgpu::ColorWrites::empty();
        if mask & 1 != 0 {
            writes |= wgpu::ColorWrites::RED;
        }
        if mask & 2 != 0 {
            writes |= wgpu::ColorWrites::GREEN;
        }
        if mask & 4 != 0 {
            writes |= wgpu::ColorWrites::BLUE;
        }
        if mask & 8 != 0 {
            writes |= wgpu::ColorWrites::ALPHA;
        }
        writes
    }

    fn wgpu_color(color: [f32; 4]) -> wgpu::Color {
        wgpu::Color {
            r: color[0] as f64,
            g: color[1] as f64,
            b: color[2] as f64,
            a: color[3] as f64,
        }
    }

    fn padded_bytes(width: u32) -> u32 {
        let unpadded = width.saturating_mul(4);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        unpadded.div_ceil(align) * align
    }

    pub(super) fn read_pixels(
        draw: &mut DrawState,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        dest: &Value,
    ) -> Result<(), u32> {
        let Some(gpu) = shared() else {
            draw.no_gpu();
            return Ok(());
        };
        let view = if bound_id(&draw.read_fbo).is_some() {
            // Read and draw framebuffers are the same attachment in this slice.
            target_size(draw)?
        } else {
            target_size(draw)?
        };
        if view.width == 0 {
            return Ok(());
        }
        if x as u32 + width as u32 > view.width || y as u32 + height as u32 > view.height {
            return Err(INVALID_OPERATION);
        }
        ensure_targets(draw, &gpu, &view)?;
        let pixels = read_color(draw, &gpu, &view)?;
        let row_pixels = if draw.pack_row_length == 0 {
            width as u32
        } else {
            draw.pack_row_length
        };
        let stride = align_up(row_pixels as usize * 4, draw.pack_alignment as usize);
        let needed = stride * height as usize;
        let Some(capacity) = w3cos_core::binary::bytes_of(dest).map(|bytes| bytes.len()) else {
            return Err(INVALID_OPERATION);
        };
        if capacity < needed {
            return Err(INVALID_OPERATION);
        }
        let mut packed = vec![0u8; needed];
        for row in 0..height as u32 {
            let gl_y = y as u32 + row;
            let gpu_y = view.height - 1 - gl_y;
            let src = (gpu_y * view.width + x as u32) as usize * 4;
            let dst = row as usize * stride;
            packed[dst..dst + width as usize * 4]
                .copy_from_slice(&pixels[src..src + width as usize * 4]);
        }
        if w3cos_core::binary::fill_array_buffer_view(dest, &packed).is_none() {
            return Err(INVALID_OPERATION);
        }
        Ok(())
    }

    pub(super) fn readback_default(draw: &mut DrawState) -> Result<(), u32> {
        let Some(gpu) = shared() else {
            return Err(INVALID_OPERATION);
        };
        let (width, height) = draw.canvas_size();
        if width == 0 || height == 0 || draw.gpu.color.is_none() {
            draw.dirty = false;
            return Ok(());
        }
        let view = View {
            width,
            height,
            has_depth: true,
            default_fb: true,
        };
        let pixels = read_color(draw, &gpu, &view)?;
        draw.revision = draw.revision.wrapping_add(1);
        draw.snapshot = Some(crate::canvas2d::CanvasSnapshot {
            width,
            height,
            pixels: std::sync::Arc::new(pixels),
            revision: draw.revision,
        });
        draw.dirty = false;
        Ok(())
    }

    fn read_color(draw: &DrawState, gpu: &SharedGpu, view: &View) -> Result<Vec<u8>, u32> {
        let texture = if view.default_fb {
            &draw.gpu.color.as_ref().ok_or(INVALID_OPERATION)?.texture
        } else {
            let fbo = bound_id(&draw.read_fbo).ok_or(INVALID_OPERATION)?;
            let color = draw
                .framebuffers
                .get(&fbo)
                .and_then(|framebuffer| framebuffer.color)
                .ok_or(INVALID_OPERATION)?;
            &draw
                .gpu
                .textures
                .get(&color)
                .ok_or(INVALID_OPERATION)?
                .texture
        };
        let bytes_per_row = padded_bytes(view.width);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("webgl readback"),
            size: u64::from(bytes_per_row) * u64::from(view.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("webgl readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(view.height),
                },
            },
            wgpu::Extent3d {
                width: view.width,
                height: view.height,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|_| INVALID_OPERATION)?;
        receiver
            .recv()
            .map_err(|_| INVALID_OPERATION)?
            .map_err(|_| INVALID_OPERATION)?;
        let mapped = slice.get_mapped_range();
        let tight = view.width as usize * 4;
        let padded = bytes_per_row as usize;
        let mut rgba = vec![0u8; tight * view.height as usize];
        for row in 0..view.height as usize {
            rgba[row * tight..(row + 1) * tight]
                .copy_from_slice(&mapped[row * padded..row * padded + tight]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(rgba)
    }
}

#[cfg(not(all(feature = "gpu", feature = "web-graphics-advanced")))]
mod gpu {
    pub(super) fn adapter_ready() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn context(width: f64, height: f64, webgl2: bool) -> Value {
        crate::webgl_web::reset();
        let canvas = Value::object(HashMap::from([
            ("width".into(), Value::Number(width)),
            ("height".into(), Value::Number(height)),
        ]));
        crate::webgl_web::context_value(canvas, webgl2)
    }

    fn call(gl: &Value, name: &str, args: Vec<Value>) -> Value {
        gl.call_method(name, args)
    }

    fn num(value: f64) -> Value {
        Value::Number(value)
    }

    fn c(gl: &Value, name: &str) -> Value {
        gl.get_property(name)
    }

    fn f32s(values: &[f32]) -> Value {
        w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Float32Array"),
            vec![Value::array(
                values.iter().map(|value| num(*value as f64)).collect(),
            )],
        )
    }

    fn u8s(values: &[u8]) -> Value {
        w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Uint8Array"),
            vec![Value::array(
                values.iter().map(|value| num(*value as f64)).collect(),
            )],
        )
    }

    fn u16s(values: &[u16]) -> Value {
        w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Uint16Array"),
            vec![Value::array(
                values.iter().map(|value| num(*value as f64)).collect(),
            )],
        )
    }

    fn shader(gl: &Value, kind: &str, source: &str) -> Value {
        let shader = call(gl, "createShader", vec![c(gl, kind)]);
        call(
            gl,
            "shaderSource",
            vec![shader.clone(), Value::string(source)],
        );
        call(gl, "compileShader", vec![shader.clone()]);
        shader
    }

    fn link_program(gl: &Value, vertex: &str, fragment: &str) -> Value {
        let vs = shader(gl, "VERTEX_SHADER", vertex);
        let fs = shader(gl, "FRAGMENT_SHADER", fragment);
        assert!(
            call(
                gl,
                "getShaderParameter",
                vec![vs.clone(), c(gl, "COMPILE_STATUS")]
            )
            .to_bool(),
            "{}",
            call(gl, "getShaderInfoLog", vec![vs.clone()]).to_js_string()
        );
        assert!(
            call(
                gl,
                "getShaderParameter",
                vec![fs.clone(), c(gl, "COMPILE_STATUS")]
            )
            .to_bool(),
            "{}",
            call(gl, "getShaderInfoLog", vec![fs.clone()]).to_js_string()
        );
        let program = call(gl, "createProgram", vec![]);
        call(gl, "attachShader", vec![program.clone(), vs]);
        call(gl, "attachShader", vec![program.clone(), fs]);
        call(gl, "linkProgram", vec![program.clone()]);
        assert!(
            call(
                gl,
                "getProgramParameter",
                vec![program.clone(), c(gl, "LINK_STATUS")]
            )
            .to_bool(),
            "{}",
            call(gl, "getProgramInfoLog", vec![program.clone()]).to_js_string()
        );
        call(gl, "useProgram", vec![program.clone()]);
        program
    }

    fn error(gl: &Value) -> f64 {
        call(gl, "getError", vec![]).to_number()
    }

    #[test]
    fn buffers_attribs_and_uniforms_keep_real_state() {
        let gl = context(8.0, 8.0, false);
        let buffer = call(&gl, "createBuffer", vec![]);
        call(
            &gl,
            "bindBuffer",
            vec![c(&gl, "ARRAY_BUFFER"), buffer.clone()],
        );
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ARRAY_BUFFER"),
                f32s(&[1.0, 2.0, 3.0, 4.0]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        assert_eq!(
            call(
                &gl,
                "getBufferParameter",
                vec![c(&gl, "ARRAY_BUFFER"), c(&gl, "BUFFER_SIZE")]
            )
            .to_number(),
            16.0
        );
        assert_eq!(
            call(
                &gl,
                "getBufferParameter",
                vec![c(&gl, "ARRAY_BUFFER"), c(&gl, "BUFFER_USAGE")]
            )
            .to_number(),
            c(&gl, "STATIC_DRAW").to_number()
        );
        call(&gl, "enableVertexAttribArray", vec![num(0.0)]);
        call(
            &gl,
            "vertexAttribPointer",
            vec![
                num(0.0),
                num(2.0),
                c(&gl, "FLOAT"),
                Value::Bool(false),
                num(0.0),
                num(0.0),
            ],
        );
        assert!(
            call(
                &gl,
                "getVertexAttrib",
                vec![num(0.0), c(&gl, "VERTEX_ATTRIB_ARRAY_ENABLED")]
            )
            .to_bool()
        );
        assert_eq!(
            call(
                &gl,
                "getVertexAttrib",
                vec![num(0.0), c(&gl, "VERTEX_ATTRIB_ARRAY_SIZE")]
            )
            .to_number(),
            2.0
        );
        call(&gl, "enable", vec![c(&gl, "DEPTH_TEST")]);
        assert!(call(&gl, "isEnabled", vec![c(&gl, "DEPTH_TEST")]).to_bool());
        call(&gl, "disable", vec![c(&gl, "DEPTH_TEST")]);
        assert!(!call(&gl, "isEnabled", vec![c(&gl, "DEPTH_TEST")]).to_bool());
        let program = link_program(
            &gl,
            "attribute vec2 a_position;\nvoid main(){ gl_Position = vec4(a_position, 0.0, 1.0); }\n",
            "precision mediump float;\nuniform vec4 u_color;\nvoid main(){ gl_FragColor = u_color; }\n",
        );
        let location = call(
            &gl,
            "getUniformLocation",
            vec![program.clone(), Value::string("u_color")],
        );
        call(
            &gl,
            "uniform4f",
            vec![location.clone(), num(0.25), num(0.5), num(0.75), num(1.0)],
        );
        let value = call(&gl, "getUniform", vec![program, location]);
        assert_eq!(value.get_property("1").to_number(), 0.5);
        assert_eq!(error(&gl), 0.0);
    }

    #[test]
    fn transform_feedback_is_rejected() {
        let gl = context(8.0, 8.0, true);
        call(&gl, "beginTransformFeedback", vec![c(&gl, "TRIANGLES")]);
        assert_eq!(error(&gl), c(&gl, "INVALID_OPERATION").to_number());
    }

    #[test]
    fn foreign_buffers_are_rejected() {
        crate::webgl_web::reset();
        let left = crate::webgl_web::context_value(Value::object(HashMap::new()), false);
        let right = crate::webgl_web::context_value(Value::object(HashMap::new()), false);
        let buffer = call(&left, "createBuffer", vec![]);
        call(
            &right,
            "bindBuffer",
            vec![c(&right, "ARRAY_BUFFER"), buffer],
        );
        assert_eq!(error(&right), c(&right, "INVALID_OPERATION").to_number());
    }

    #[test]
    fn read_pixels_does_not_invent_values_without_an_adapter() {
        if gpu_tests_enabled() {
            return;
        }
        let gl = context(4.0, 4.0, false);
        let pixels = u8s(&[7u8; 16]);
        call(
            &gl,
            "clearColor",
            vec![num(1.0), num(0.0), num(0.0), num(1.0)],
        );
        call(&gl, "clear", vec![c(&gl, "COLOR_BUFFER_BIT")]);
        call(
            &gl,
            "readPixels",
            vec![
                num(0.0),
                num(0.0),
                num(2.0),
                num(2.0),
                c(&gl, "RGBA"),
                c(&gl, "UNSIGNED_BYTE"),
                pixels.clone(),
            ],
        );
        assert_eq!(error(&gl), c(&gl, "INVALID_OPERATION").to_number());
        assert!(
            w3cos_core::binary::bytes_of(&pixels)
                .unwrap()
                .iter()
                .all(|byte| *byte == 7)
        );
    }

    fn gpu_tests_enabled() -> bool {
        #[cfg(all(feature = "gpu", feature = "web-graphics-advanced"))]
        {
            if super::gpu::adapter_ready() {
                return true;
            }
        }
        if std::env::var("W3COS_GPU_TESTS").ok().as_deref() == Some("1") {
            panic!("W3COS_GPU_TESTS=1 but no wgpu adapter is available");
        }
        eprintln!("[w3cos] skipping WebGL GPU test: no adapter");
        false
    }

    fn pixel(bytes: &[u8], x: u32, y: u32, width: u32) -> [u8; 4] {
        let index = ((y * width + x) * 4) as usize;
        [
            bytes[index],
            bytes[index + 1],
            bytes[index + 2],
            bytes[index + 3],
        ]
    }

    fn expect_near(actual: [u8; 4], expected: [u8; 4], tolerance: u8) {
        for index in 0..4 {
            let delta = actual[index].abs_diff(expected[index]);
            assert!(
                delta <= tolerance,
                "pixel {actual:?} != {expected:?} (tolerance {tolerance})"
            );
        }
    }

    fn read(gl: &Value, width: u32, height: u32) -> Vec<u8> {
        let pixels = u8s(&vec![0u8; (width * height * 4) as usize]);
        call(
            gl,
            "readPixels",
            vec![
                num(0.0),
                num(0.0),
                num(width as f64),
                num(height as f64),
                c(gl, "RGBA"),
                c(gl, "UNSIGNED_BYTE"),
                pixels.clone(),
            ],
        );
        assert_eq!(error(gl), 0.0, "readPixels recorded an error");
        w3cos_core::binary::bytes_of(&pixels).unwrap()
    }

    #[test]
    fn gpu_clear_triangle_texture_depth_blend_and_scissor() {
        if !gpu_tests_enabled() {
            return;
        }
        let gl = context(8.0, 8.0, true);
        call(
            &gl,
            "clearColor",
            vec![num(1.0), num(0.0), num(0.0), num(1.0)],
        );
        call(&gl, "clear", vec![c(&gl, "COLOR_BUFFER_BIT")]);
        expect_near(pixel(&read(&gl, 8, 8), 1, 1, 8), [255, 0, 0, 255], 1);

        call(
            &gl,
            "clearColor",
            vec![num(0.0), num(0.0), num(0.0), num(1.0)],
        );
        call(&gl, "clear", vec![c(&gl, "COLOR_BUFFER_BIT")]);
        call(&gl, "enable", vec![c(&gl, "SCISSOR_TEST")]);
        call(&gl, "scissor", vec![num(0.0), num(0.0), num(8.0), num(4.0)]);
        call(
            &gl,
            "clearColor",
            vec![num(1.0), num(0.0), num(0.0), num(1.0)],
        );
        call(&gl, "clear", vec![c(&gl, "COLOR_BUFFER_BIT")]);
        let scissored = read(&gl, 8, 8);
        expect_near(pixel(&scissored, 4, 0, 8), [255, 0, 0, 255], 1);
        expect_near(pixel(&scissored, 4, 7, 8), [0, 0, 0, 255], 1);
        call(&gl, "disable", vec![c(&gl, "SCISSOR_TEST")]);

        let program = link_program(
            &gl,
            "attribute vec2 a_position;\nvoid main(){ gl_Position = vec4(a_position, 0.0, 1.0); }\n",
            "precision mediump float;\nuniform vec4 u_color;\nvoid main(){ gl_FragColor = u_color; }\n",
        );
        let color = call(
            &gl,
            "getUniformLocation",
            vec![program, Value::string("u_color")],
        );
        let buffer = call(&gl, "createBuffer", vec![]);
        call(&gl, "bindBuffer", vec![c(&gl, "ARRAY_BUFFER"), buffer]);
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ARRAY_BUFFER"),
                f32s(&[-1.0, -0.5, 1.0, -0.5, 0.0, 1.0]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        call(&gl, "enableVertexAttribArray", vec![num(0.0)]);
        call(
            &gl,
            "vertexAttribPointer",
            vec![
                num(0.0),
                num(2.0),
                c(&gl, "FLOAT"),
                Value::Bool(false),
                num(0.0),
                num(0.0),
            ],
        );
        call(
            &gl,
            "clearColor",
            vec![num(0.0), num(0.0), num(0.0), num(1.0)],
        );
        call(&gl, "clear", vec![c(&gl, "COLOR_BUFFER_BIT")]);
        call(
            &gl,
            "uniform4f",
            vec![color.clone(), num(0.0), num(1.0), num(0.0), num(1.0)],
        );
        call(
            &gl,
            "drawArrays",
            vec![c(&gl, "TRIANGLES"), num(0.0), num(3.0)],
        );
        let triangle = read(&gl, 8, 8);
        expect_near(pixel(&triangle, 4, 6, 8), [0, 255, 0, 255], 8);
        expect_near(pixel(&triangle, 4, 0, 8), [0, 0, 0, 255], 8);

        let textured = link_program(
            &gl,
            "attribute vec2 a_position;\nattribute vec2 a_uv;\nvarying vec2 v_uv;\n\
             void main(){ v_uv = a_uv; gl_Position = vec4(a_position, 0.0, 1.0); }\n",
            "precision mediump float;\nuniform sampler2D u_tex;\nvarying vec2 v_uv;\n\
             void main(){ gl_FragColor = texture2D(u_tex, v_uv); }\n",
        );
        let texture = call(&gl, "createTexture", vec![]);
        call(&gl, "bindTexture", vec![c(&gl, "TEXTURE_2D"), texture]);
        call(
            &gl,
            "texParameteri",
            vec![
                c(&gl, "TEXTURE_2D"),
                c(&gl, "TEXTURE_MIN_FILTER"),
                c(&gl, "NEAREST"),
            ],
        );
        call(
            &gl,
            "texParameteri",
            vec![
                c(&gl, "TEXTURE_2D"),
                c(&gl, "TEXTURE_MAG_FILTER"),
                c(&gl, "NEAREST"),
            ],
        );
        call(
            &gl,
            "texImage2D",
            vec![
                c(&gl, "TEXTURE_2D"),
                num(0.0),
                c(&gl, "RGBA"),
                num(1.0),
                num(1.0),
                num(0.0),
                c(&gl, "RGBA"),
                c(&gl, "UNSIGNED_BYTE"),
                u8s(&[10, 20, 30, 255]),
            ],
        );
        let positions = call(&gl, "createBuffer", vec![]);
        call(&gl, "bindBuffer", vec![c(&gl, "ARRAY_BUFFER"), positions]);
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ARRAY_BUFFER"),
                f32s(&[-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        let pos = call(
            &gl,
            "getAttribLocation",
            vec![textured.clone(), Value::string("a_position")],
        );
        call(&gl, "enableVertexAttribArray", vec![pos.clone()]);
        call(
            &gl,
            "vertexAttribPointer",
            vec![
                pos,
                num(2.0),
                c(&gl, "FLOAT"),
                Value::Bool(false),
                num(0.0),
                num(0.0),
            ],
        );
        let uvs = call(&gl, "createBuffer", vec![]);
        call(&gl, "bindBuffer", vec![c(&gl, "ARRAY_BUFFER"), uvs]);
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ARRAY_BUFFER"),
                f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        let uv = call(
            &gl,
            "getAttribLocation",
            vec![textured.clone(), Value::string("a_uv")],
        );
        call(&gl, "enableVertexAttribArray", vec![uv.clone()]);
        call(
            &gl,
            "vertexAttribPointer",
            vec![
                uv,
                num(2.0),
                c(&gl, "FLOAT"),
                Value::Bool(false),
                num(0.0),
                num(0.0),
            ],
        );
        let indices = call(&gl, "createBuffer", vec![]);
        call(
            &gl,
            "bindBuffer",
            vec![c(&gl, "ELEMENT_ARRAY_BUFFER"), indices],
        );
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ELEMENT_ARRAY_BUFFER"),
                u16s(&[0, 1, 2, 2, 1, 3]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        let sampler = call(
            &gl,
            "getUniformLocation",
            vec![textured, Value::string("u_tex")],
        );
        call(&gl, "uniform1i", vec![sampler, num(0.0)]);
        call(
            &gl,
            "drawElements",
            vec![
                c(&gl, "TRIANGLES"),
                num(6.0),
                c(&gl, "UNSIGNED_SHORT"),
                num(0.0),
            ],
        );
        expect_near(pixel(&read(&gl, 8, 8), 3, 3, 8), [10, 20, 30, 255], 2);

        let depth = link_program(
            &gl,
            "attribute vec2 a_position;\nattribute float a_z;\n\
             void main(){ gl_Position = vec4(a_position, a_z, 1.0); }\n",
            "precision mediump float;\nuniform vec4 u_color;\nvoid main(){ gl_FragColor = u_color; }\n",
        );
        let depth_color = call(
            &gl,
            "getUniformLocation",
            vec![depth.clone(), Value::string("u_color")],
        );
        let geometry = call(&gl, "createBuffer", vec![]);
        call(&gl, "bindBuffer", vec![c(&gl, "ARRAY_BUFFER"), geometry]);
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ARRAY_BUFFER"),
                f32s(&[-1.0, -1.0, 0.6, 3.0, -1.0, 0.6, -1.0, 3.0, 0.6]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        let pos = call(
            &gl,
            "getAttribLocation",
            vec![depth.clone(), Value::string("a_position")],
        );
        let z = call(&gl, "getAttribLocation", vec![depth, Value::string("a_z")]);
        call(&gl, "enableVertexAttribArray", vec![pos.clone()]);
        call(
            &gl,
            "vertexAttribPointer",
            vec![
                pos,
                num(2.0),
                c(&gl, "FLOAT"),
                Value::Bool(false),
                num(12.0),
                num(0.0),
            ],
        );
        call(&gl, "enableVertexAttribArray", vec![z.clone()]);
        call(
            &gl,
            "vertexAttribPointer",
            vec![
                z,
                num(1.0),
                c(&gl, "FLOAT"),
                Value::Bool(false),
                num(12.0),
                num(8.0),
            ],
        );
        call(
            &gl,
            "clearColor",
            vec![num(0.0), num(0.0), num(0.0), num(1.0)],
        );
        call(&gl, "clearDepth", vec![num(1.0)]);
        call(
            &gl,
            "clear",
            vec![num(
                c(&gl, "COLOR_BUFFER_BIT").to_number() + c(&gl, "DEPTH_BUFFER_BIT").to_number()
            )],
        );
        call(&gl, "enable", vec![c(&gl, "DEPTH_TEST")]);
        call(&gl, "depthFunc", vec![c(&gl, "LESS")]);
        call(
            &gl,
            "uniform4f",
            vec![depth_color.clone(), num(1.0), num(0.0), num(0.0), num(1.0)],
        );
        call(
            &gl,
            "drawArrays",
            vec![c(&gl, "TRIANGLES"), num(0.0), num(3.0)],
        );
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ARRAY_BUFFER"),
                f32s(&[-1.0, -1.0, -0.6, 3.0, -1.0, -0.6, -1.0, 3.0, -0.6]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        call(
            &gl,
            "uniform4f",
            vec![depth_color, num(0.0), num(1.0), num(0.0), num(1.0)],
        );
        call(
            &gl,
            "drawArrays",
            vec![c(&gl, "TRIANGLES"), num(0.0), num(3.0)],
        );
        expect_near(pixel(&read(&gl, 8, 8), 2, 2, 8), [0, 255, 0, 255], 8);
        call(&gl, "disable", vec![c(&gl, "DEPTH_TEST")]);

        call(&gl, "enable", vec![c(&gl, "BLEND")]);
        call(
            &gl,
            "blendFunc",
            vec![c(&gl, "SRC_ALPHA"), c(&gl, "ONE_MINUS_SRC_ALPHA")],
        );
        call(
            &gl,
            "clearColor",
            vec![num(0.0), num(1.0), num(0.0), num(1.0)],
        );
        call(&gl, "clear", vec![c(&gl, "COLOR_BUFFER_BIT")]);
        let solid = link_program(
            &gl,
            "attribute vec2 a_position;\nvoid main(){ gl_Position = vec4(a_position, 0.0, 1.0); }\n",
            "precision mediump float;\nuniform vec4 u_color;\nvoid main(){ gl_FragColor = u_color; }\n",
        );
        let blend_color = call(
            &gl,
            "getUniformLocation",
            vec![solid, Value::string("u_color")],
        );
        let cover = call(&gl, "createBuffer", vec![]);
        call(&gl, "bindBuffer", vec![c(&gl, "ARRAY_BUFFER"), cover]);
        call(
            &gl,
            "bufferData",
            vec![
                c(&gl, "ARRAY_BUFFER"),
                f32s(&[-1.0, -1.0, 3.0, -1.0, -1.0, 3.0]),
                c(&gl, "STATIC_DRAW"),
            ],
        );
        call(&gl, "enableVertexAttribArray", vec![num(0.0)]);
        call(
            &gl,
            "vertexAttribPointer",
            vec![
                num(0.0),
                num(2.0),
                c(&gl, "FLOAT"),
                Value::Bool(false),
                num(0.0),
                num(0.0),
            ],
        );
        call(
            &gl,
            "uniform4f",
            vec![blend_color, num(0.0), num(0.0), num(1.0), num(0.5)],
        );
        call(
            &gl,
            "drawArrays",
            vec![c(&gl, "TRIANGLES"), num(0.0), num(3.0)],
        );
        expect_near(pixel(&read(&gl, 8, 8), 4, 4, 8), [0, 128, 128, 191], 12);
    }

    #[test]
    fn gpu_present_publishes_drawing_buffer_pixels() {
        if !gpu_tests_enabled() {
            return;
        }
        crate::dom::reset_document();
        crate::webgl_web::reset();
        let id = crate::dom::create_element("canvas");
        crate::dom::set_attribute(id, "width", "4");
        crate::dom::set_attribute(id, "height", "4");
        let canvas = crate::jsdom::element_value(id);
        let gl = crate::webgl_web::context_value(canvas, false);
        call(
            &gl,
            "clearColor",
            vec![num(1.0), num(0.0), num(0.0), num(1.0)],
        );
        call(&gl, "clear", vec![c(&gl, "COLOR_BUFFER_BIT")]);
        assert_eq!(error(&gl), 0.0);
        publish_drawing_buffers([(9, id)]);
        let snapshot = crate::canvas2d::surface_snapshot(9).expect("canvas snapshot");
        assert_eq!((snapshot.width, snapshot.height), (4, 4));
        expect_near(
            [
                snapshot.pixels[0],
                snapshot.pixels[1],
                snapshot.pixels[2],
                snapshot.pixels[3],
            ],
            [255, 0, 0, 255],
            1,
        );
    }
}
