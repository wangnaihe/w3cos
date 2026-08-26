//! Stateful WebGL 1/2 compatibility contexts.
//!
//! Resource identity, shader/program lifecycle, state queries and standard
//! constants are available. Draw/upload calls preserve state and warn because
//! GLSL-to-wgpu translation is not yet connected to the compositor.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static CONTEXTS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static OBJECTS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn realm_webgl_function(callback: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), callback)
}

const WEBGL1_METHODS: &str = "
activeTexture attachShader bindAttribLocation bindBuffer bindFramebuffer bindRenderbuffer
bindTexture blendColor blendEquation blendEquationSeparate blendFunc blendFuncSeparate bufferData
bufferSubData checkFramebufferStatus clear clearColor clearDepth clearStencil colorMask compileShader
compressedTexImage2D compressedTexSubImage2D copyTexImage2D copyTexSubImage2D createBuffer
createFramebuffer createProgram createRenderbuffer createShader createTexture cullFace deleteBuffer
deleteFramebuffer deleteProgram deleteRenderbuffer deleteShader deleteTexture depthFunc depthMask
depthRange detachShader disable disableVertexAttribArray drawArrays drawElements drawingBufferStorage
enable enableVertexAttribArray finish flush framebufferRenderbuffer framebufferTexture2D frontFace
generateMipmap getActiveAttrib getActiveUniform getAttachedShaders getAttribLocation getBufferParameter
getContextAttributes getError getExtension getFramebufferAttachmentParameter getParameter
getProgramInfoLog getProgramParameter getRenderbufferParameter getShaderInfoLog getShaderParameter
getShaderPrecisionFormat getShaderSource getSupportedExtensions getTexParameter getUniform
getUniformLocation getVertexAttrib getVertexAttribOffset hint isBuffer isContextLost isEnabled
isFramebuffer isProgram isRenderbuffer isShader isTexture lineWidth linkProgram makeXRCompatible
pixelStorei polygonOffset readPixels renderbufferStorage sampleCoverage scissor shaderSource
stencilFunc stencilFuncSeparate stencilMask stencilMaskSeparate stencilOp stencilOpSeparate texImage2D
texParameterf texParameteri texSubImage2D uniform1f uniform1fv uniform1i uniform1iv uniform2f uniform2fv
uniform2i uniform2iv uniform3f uniform3fv uniform3i uniform3iv uniform4f uniform4fv uniform4i uniform4iv
uniformMatrix2fv uniformMatrix3fv uniformMatrix4fv useProgram validateProgram vertexAttrib1f vertexAttrib1fv
vertexAttrib2f vertexAttrib2fv vertexAttrib3f vertexAttrib3fv vertexAttrib4f vertexAttrib4fv
vertexAttribPointer viewport";

const WEBGL2_METHODS: &str = "
activeTexture attachShader beginQuery beginTransformFeedback bindAttribLocation bindBuffer
bindBufferBase bindBufferRange bindFramebuffer bindRenderbuffer bindSampler bindTexture
bindTransformFeedback bindVertexArray blendColor blendEquation blendEquationSeparate blendFunc
blendFuncSeparate blitFramebuffer bufferData bufferSubData checkFramebufferStatus clear clearBufferfi
clearBufferfv clearBufferiv clearBufferuiv clearColor clearDepth clearStencil clientWaitSync colorMask
compileShader compressedTexImage2D compressedTexImage3D compressedTexSubImage2D compressedTexSubImage3D
copyBufferSubData copyTexImage2D copyTexSubImage2D copyTexSubImage3D createBuffer createFramebuffer
createProgram createQuery createRenderbuffer createSampler createShader createTexture
createTransformFeedback createVertexArray cullFace deleteBuffer deleteFramebuffer deleteProgram
deleteQuery deleteRenderbuffer deleteSampler deleteShader deleteSync deleteTexture deleteTransformFeedback
deleteVertexArray depthFunc depthMask depthRange detachShader disable disableVertexAttribArray drawArrays
drawArraysInstanced drawBuffers drawElements drawElementsInstanced drawRangeElements drawingBufferStorage
enable enableVertexAttribArray endQuery endTransformFeedback fenceSync finish flush framebufferRenderbuffer
framebufferTexture2D framebufferTextureLayer frontFace generateMipmap getActiveAttrib getActiveUniform
getActiveUniformBlockName getActiveUniformBlockParameter getActiveUniforms getAttachedShaders
getAttribLocation getBufferParameter getBufferSubData getContextAttributes getError getExtension
getFragDataLocation getFramebufferAttachmentParameter getIndexedParameter getInternalformatParameter
getParameter getProgramInfoLog getProgramParameter getQuery getQueryParameter getRenderbufferParameter
getSamplerParameter getShaderInfoLog getShaderParameter getShaderPrecisionFormat getShaderSource
getSupportedExtensions getSyncParameter getTexParameter getTransformFeedbackVarying getUniform
getUniformBlockIndex getUniformIndices getUniformLocation getVertexAttrib getVertexAttribOffset hint
invalidateFramebuffer invalidateSubFramebuffer isBuffer isContextLost isEnabled isFramebuffer isProgram
isQuery isRenderbuffer isSampler isShader isSync isTexture isTransformFeedback isVertexArray lineWidth
linkProgram makeXRCompatible pauseTransformFeedback pixelStorei polygonOffset readBuffer readPixels
renderbufferStorage renderbufferStorageMultisample resumeTransformFeedback sampleCoverage samplerParameterf
samplerParameteri scissor shaderSource stencilFunc stencilFuncSeparate stencilMask stencilMaskSeparate
stencilOp stencilOpSeparate texImage2D texImage3D texParameterf texParameteri texStorage2D texStorage3D
texSubImage2D texSubImage3D transformFeedbackVaryings uniform1f uniform1fv uniform1i uniform1iv uniform1ui
uniform1uiv uniform2f uniform2fv uniform2i uniform2iv uniform2ui uniform2uiv uniform3f uniform3fv uniform3i
uniform3iv uniform3ui uniform3uiv uniform4f uniform4fv uniform4i uniform4iv uniform4ui uniform4uiv
uniformBlockBinding uniformMatrix2fv uniformMatrix2x3fv uniformMatrix2x4fv uniformMatrix3fv
uniformMatrix3x2fv uniformMatrix3x4fv uniformMatrix4fv uniformMatrix4x2fv uniformMatrix4x3fv useProgram
validateProgram vertexAttrib1f vertexAttrib1fv vertexAttrib2f vertexAttrib2fv vertexAttrib3f vertexAttrib3fv
vertexAttrib4f vertexAttrib4fv vertexAttribDivisor vertexAttribI4i vertexAttribI4iv vertexAttribI4ui
vertexAttribI4uiv vertexAttribIPointer vertexAttribPointer viewport waitSync";

const WEBGL1_CONSTANTS: &str = "
ACTIVE_ATTRIBUTES ACTIVE_TEXTURE ACTIVE_UNIFORMS ALIASED_LINE_WIDTH_RANGE ALIASED_POINT_SIZE_RANGE ALPHA
ALPHA_BITS ALWAYS ARRAY_BUFFER ARRAY_BUFFER_BINDING ATTACHED_SHADERS BACK BLEND BLEND_COLOR BLEND_DST_ALPHA
BLEND_DST_RGB BLEND_EQUATION BLEND_EQUATION_ALPHA BLEND_EQUATION_RGB BLEND_SRC_ALPHA BLEND_SRC_RGB BLUE_BITS
BOOL BOOL_VEC2 BOOL_VEC3 BOOL_VEC4 BROWSER_DEFAULT_WEBGL BUFFER_SIZE BUFFER_USAGE BYTE CCW CLAMP_TO_EDGE
COLOR_ATTACHMENT0 COLOR_BUFFER_BIT COLOR_CLEAR_VALUE COLOR_WRITEMASK COMPILE_STATUS
COMPRESSED_TEXTURE_FORMATS CONSTANT_ALPHA CONSTANT_COLOR CONTEXT_LOST_WEBGL CULL_FACE CULL_FACE_MODE
CURRENT_PROGRAM CURRENT_VERTEX_ATTRIB CW DECR DECR_WRAP DELETE_STATUS DEPTH_ATTACHMENT DEPTH_BITS
DEPTH_BUFFER_BIT DEPTH_CLEAR_VALUE DEPTH_COMPONENT DEPTH_COMPONENT16 DEPTH_FUNC DEPTH_RANGE DEPTH_STENCIL
DEPTH_STENCIL_ATTACHMENT DEPTH_TEST DEPTH_WRITEMASK DITHER DONT_CARE DST_ALPHA DST_COLOR DYNAMIC_DRAW
ELEMENT_ARRAY_BUFFER ELEMENT_ARRAY_BUFFER_BINDING EQUAL FASTEST FLOAT FLOAT_MAT2 FLOAT_MAT3 FLOAT_MAT4
FLOAT_VEC2 FLOAT_VEC3 FLOAT_VEC4 FRAGMENT_SHADER FRAMEBUFFER FRAMEBUFFER_ATTACHMENT_OBJECT_NAME
FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE
FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL FRAMEBUFFER_BINDING FRAMEBUFFER_COMPLETE
FRAMEBUFFER_INCOMPLETE_ATTACHMENT FRAMEBUFFER_INCOMPLETE_DIMENSIONS FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT
FRAMEBUFFER_UNSUPPORTED FRONT FRONT_AND_BACK FRONT_FACE FUNC_ADD FUNC_REVERSE_SUBTRACT FUNC_SUBTRACT
GENERATE_MIPMAP_HINT GEQUAL GREATER GREEN_BITS HIGH_FLOAT HIGH_INT IMPLEMENTATION_COLOR_READ_FORMAT
IMPLEMENTATION_COLOR_READ_TYPE INCR INCR_WRAP INT INT_VEC2 INT_VEC3 INT_VEC4 INVALID_ENUM
INVALID_FRAMEBUFFER_OPERATION INVALID_OPERATION INVALID_VALUE INVERT KEEP LEQUAL LESS LINEAR
LINEAR_MIPMAP_LINEAR LINEAR_MIPMAP_NEAREST LINES LINE_LOOP LINE_STRIP LINE_WIDTH LINK_STATUS LOW_FLOAT LOW_INT
LUMINANCE LUMINANCE_ALPHA MAX_COMBINED_TEXTURE_IMAGE_UNITS MAX_CUBE_MAP_TEXTURE_SIZE
MAX_FRAGMENT_UNIFORM_VECTORS MAX_RENDERBUFFER_SIZE MAX_TEXTURE_IMAGE_UNITS MAX_TEXTURE_SIZE
MAX_VARYING_VECTORS MAX_VERTEX_ATTRIBS MAX_VERTEX_TEXTURE_IMAGE_UNITS MAX_VERTEX_UNIFORM_VECTORS
MAX_VIEWPORT_DIMS MEDIUM_FLOAT MEDIUM_INT MIRRORED_REPEAT NEAREST NEAREST_MIPMAP_LINEAR
NEAREST_MIPMAP_NEAREST NEVER NICEST NONE NOTEQUAL NO_ERROR ONE ONE_MINUS_CONSTANT_ALPHA
ONE_MINUS_CONSTANT_COLOR ONE_MINUS_DST_ALPHA ONE_MINUS_DST_COLOR ONE_MINUS_SRC_ALPHA ONE_MINUS_SRC_COLOR
OUT_OF_MEMORY PACK_ALIGNMENT POINTS POLYGON_OFFSET_FACTOR POLYGON_OFFSET_FILL POLYGON_OFFSET_UNITS RED_BITS
RENDERBUFFER RENDERBUFFER_ALPHA_SIZE RENDERBUFFER_BINDING RENDERBUFFER_BLUE_SIZE RENDERBUFFER_DEPTH_SIZE
RENDERBUFFER_GREEN_SIZE RENDERBUFFER_HEIGHT RENDERBUFFER_INTERNAL_FORMAT RENDERBUFFER_RED_SIZE
RENDERBUFFER_STENCIL_SIZE RENDERBUFFER_WIDTH RENDERER REPEAT REPLACE RGB RGB565 RGB5_A1 RGB8 RGBA RGBA4 RGBA8
SAMPLER_2D SAMPLER_CUBE SAMPLES SAMPLE_ALPHA_TO_COVERAGE SAMPLE_BUFFERS SAMPLE_COVERAGE
SAMPLE_COVERAGE_INVERT SAMPLE_COVERAGE_VALUE SCISSOR_BOX SCISSOR_TEST SHADER_TYPE SHADING_LANGUAGE_VERSION
SHORT SRC_ALPHA SRC_ALPHA_SATURATE SRC_COLOR STATIC_DRAW STENCIL_ATTACHMENT STENCIL_BACK_FAIL
STENCIL_BACK_FUNC STENCIL_BACK_PASS_DEPTH_FAIL STENCIL_BACK_PASS_DEPTH_PASS STENCIL_BACK_REF
STENCIL_BACK_VALUE_MASK STENCIL_BACK_WRITEMASK STENCIL_BITS STENCIL_BUFFER_BIT STENCIL_CLEAR_VALUE STENCIL_FAIL
STENCIL_FUNC STENCIL_INDEX8 STENCIL_PASS_DEPTH_FAIL STENCIL_PASS_DEPTH_PASS STENCIL_REF STENCIL_TEST
STENCIL_VALUE_MASK STENCIL_WRITEMASK STREAM_DRAW SUBPIXEL_BITS TEXTURE TEXTURE0 TEXTURE1 TEXTURE10 TEXTURE11
TEXTURE12 TEXTURE13 TEXTURE14 TEXTURE15 TEXTURE16 TEXTURE17 TEXTURE18 TEXTURE19 TEXTURE2 TEXTURE20 TEXTURE21
TEXTURE22 TEXTURE23 TEXTURE24 TEXTURE25 TEXTURE26 TEXTURE27 TEXTURE28 TEXTURE29 TEXTURE3 TEXTURE30 TEXTURE31
TEXTURE4 TEXTURE5 TEXTURE6 TEXTURE7 TEXTURE8 TEXTURE9 TEXTURE_2D TEXTURE_BINDING_2D
TEXTURE_BINDING_CUBE_MAP TEXTURE_CUBE_MAP TEXTURE_CUBE_MAP_NEGATIVE_X TEXTURE_CUBE_MAP_NEGATIVE_Y
TEXTURE_CUBE_MAP_NEGATIVE_Z TEXTURE_CUBE_MAP_POSITIVE_X TEXTURE_CUBE_MAP_POSITIVE_Y
TEXTURE_CUBE_MAP_POSITIVE_Z TEXTURE_MAG_FILTER TEXTURE_MIN_FILTER TEXTURE_WRAP_S TEXTURE_WRAP_T TRIANGLES
TRIANGLE_FAN TRIANGLE_STRIP UNPACK_ALIGNMENT UNPACK_COLORSPACE_CONVERSION_WEBGL UNPACK_FLIP_Y_WEBGL
UNPACK_PREMULTIPLY_ALPHA_WEBGL UNSIGNED_BYTE UNSIGNED_INT UNSIGNED_SHORT UNSIGNED_SHORT_4_4_4_4
UNSIGNED_SHORT_5_5_5_1 UNSIGNED_SHORT_5_6_5 VALIDATE_STATUS VENDOR VERSION
VERTEX_ATTRIB_ARRAY_BUFFER_BINDING VERTEX_ATTRIB_ARRAY_ENABLED VERTEX_ATTRIB_ARRAY_NORMALIZED
VERTEX_ATTRIB_ARRAY_POINTER VERTEX_ATTRIB_ARRAY_SIZE VERTEX_ATTRIB_ARRAY_STRIDE VERTEX_ATTRIB_ARRAY_TYPE
VERTEX_SHADER VIEWPORT ZERO";

const WEBGL2_EXTRA_CONSTANTS: &str = "
ACTIVE_UNIFORM_BLOCKS ALREADY_SIGNALED ANY_SAMPLES_PASSED ANY_SAMPLES_PASSED_CONSERVATIVE COLOR
COLOR_ATTACHMENT1 COLOR_ATTACHMENT2 COLOR_ATTACHMENT3 COLOR_ATTACHMENT4 COLOR_ATTACHMENT5
COLOR_ATTACHMENT6 COLOR_ATTACHMENT7 COLOR_ATTACHMENT8 COLOR_ATTACHMENT9 COLOR_ATTACHMENT10
COLOR_ATTACHMENT11 COLOR_ATTACHMENT12 COLOR_ATTACHMENT13 COLOR_ATTACHMENT14 COLOR_ATTACHMENT15
COMPARE_REF_TO_TEXTURE CONDITION_SATISFIED COPY_READ_BUFFER COPY_READ_BUFFER_BINDING COPY_WRITE_BUFFER
COPY_WRITE_BUFFER_BINDING CURRENT_QUERY DEPTH DEPTH24_STENCIL8 DEPTH32F_STENCIL8 DEPTH_COMPONENT24
DEPTH_COMPONENT32F DRAW_BUFFER0 DRAW_BUFFER1 DRAW_BUFFER2 DRAW_BUFFER3 DRAW_BUFFER4 DRAW_BUFFER5 DRAW_BUFFER6
DRAW_BUFFER7 DRAW_BUFFER8 DRAW_BUFFER9 DRAW_BUFFER10 DRAW_BUFFER11 DRAW_BUFFER12 DRAW_BUFFER13 DRAW_BUFFER14
DRAW_BUFFER15 DRAW_FRAMEBUFFER DRAW_FRAMEBUFFER_BINDING DYNAMIC_COPY DYNAMIC_READ
FLOAT_32_UNSIGNED_INT_24_8_REV FLOAT_MAT2x3 FLOAT_MAT2x4 FLOAT_MAT3x2 FLOAT_MAT3x4 FLOAT_MAT4x2 FLOAT_MAT4x3
FRAGMENT_SHADER_DERIVATIVE_HINT FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE FRAMEBUFFER_ATTACHMENT_BLUE_SIZE
FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE
FRAMEBUFFER_ATTACHMENT_GREEN_SIZE FRAMEBUFFER_ATTACHMENT_RED_SIZE FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE
FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER FRAMEBUFFER_DEFAULT FRAMEBUFFER_INCOMPLETE_MULTISAMPLE HALF_FLOAT
INTERLEAVED_ATTRIBS INT_2_10_10_10_REV INT_SAMPLER_2D INT_SAMPLER_2D_ARRAY INT_SAMPLER_3D INT_SAMPLER_CUBE
INVALID_INDEX MAX MAX_3D_TEXTURE_SIZE MAX_ARRAY_TEXTURE_LAYERS MAX_CLIENT_WAIT_TIMEOUT_WEBGL
MAX_COLOR_ATTACHMENTS MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS MAX_COMBINED_UNIFORM_BLOCKS
MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS MAX_DRAW_BUFFERS MAX_ELEMENTS_INDICES MAX_ELEMENTS_VERTICES
MAX_ELEMENT_INDEX MAX_FRAGMENT_INPUT_COMPONENTS MAX_FRAGMENT_UNIFORM_BLOCKS MAX_FRAGMENT_UNIFORM_COMPONENTS
MAX_PROGRAM_TEXEL_OFFSET MAX_SAMPLES MAX_SERVER_WAIT_TIMEOUT MAX_TEXTURE_LOD_BIAS
MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS
MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS MAX_UNIFORM_BLOCK_SIZE MAX_UNIFORM_BUFFER_BINDINGS
MAX_VARYING_COMPONENTS MAX_VERTEX_OUTPUT_COMPONENTS MAX_VERTEX_UNIFORM_BLOCKS MAX_VERTEX_UNIFORM_COMPONENTS
MIN MIN_PROGRAM_TEXEL_OFFSET OBJECT_TYPE PACK_ROW_LENGTH PACK_SKIP_PIXELS PACK_SKIP_ROWS PIXEL_PACK_BUFFER
PIXEL_PACK_BUFFER_BINDING PIXEL_UNPACK_BUFFER PIXEL_UNPACK_BUFFER_BINDING QUERY_RESULT QUERY_RESULT_AVAILABLE
R11F_G11F_B10F R16F R16I R16UI R32F R32I R32UI R8 R8I R8UI R8_SNORM RASTERIZER_DISCARD READ_BUFFER
READ_FRAMEBUFFER READ_FRAMEBUFFER_BINDING RED RED_INTEGER RENDERBUFFER_SAMPLES RG RG16F RG16I RG16UI RG32F
RG32I RG32UI RG8 RG8I RG8UI RG8_SNORM RGB10_A2 RGB10_A2UI RGB16F RGB16I RGB16UI RGB32F RGB32I RGB32UI
RGB8I RGB8UI RGB8_SNORM RGB9_E5 RGBA16F RGBA16I RGBA16UI RGBA32F RGBA32I RGBA32UI RGBA8I RGBA8UI
RGBA8_SNORM RGBA_INTEGER RGB_INTEGER RG_INTEGER SAMPLER_2D_ARRAY SAMPLER_2D_ARRAY_SHADOW SAMPLER_2D_SHADOW
SAMPLER_3D SAMPLER_BINDING SAMPLER_CUBE_SHADOW SEPARATE_ATTRIBS SIGNALED SIGNED_NORMALIZED SRGB SRGB8
SRGB8_ALPHA8 STATIC_COPY STATIC_READ STENCIL STREAM_COPY STREAM_READ SYNC_CONDITION SYNC_FENCE SYNC_FLAGS
SYNC_FLUSH_COMMANDS_BIT SYNC_GPU_COMMANDS_COMPLETE SYNC_STATUS TEXTURE_2D_ARRAY TEXTURE_3D TEXTURE_BASE_LEVEL
TEXTURE_BINDING_2D_ARRAY TEXTURE_BINDING_3D TEXTURE_COMPARE_FUNC TEXTURE_COMPARE_MODE TEXTURE_IMMUTABLE_FORMAT
TEXTURE_IMMUTABLE_LEVELS TEXTURE_MAX_LEVEL TEXTURE_MAX_LOD TEXTURE_MIN_LOD TEXTURE_WRAP_R TIMEOUT_EXPIRED
TIMEOUT_IGNORED TRANSFORM_FEEDBACK TRANSFORM_FEEDBACK_ACTIVE TRANSFORM_FEEDBACK_BINDING
TRANSFORM_FEEDBACK_BUFFER TRANSFORM_FEEDBACK_BUFFER_BINDING TRANSFORM_FEEDBACK_BUFFER_MODE
TRANSFORM_FEEDBACK_BUFFER_SIZE TRANSFORM_FEEDBACK_BUFFER_START TRANSFORM_FEEDBACK_PAUSED
TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN TRANSFORM_FEEDBACK_VARYINGS UNIFORM_ARRAY_STRIDE
UNIFORM_BLOCK_ACTIVE_UNIFORMS UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES UNIFORM_BLOCK_BINDING
UNIFORM_BLOCK_DATA_SIZE UNIFORM_BLOCK_INDEX UNIFORM_BLOCK_REFERENCED_BY_FRAGMENT_SHADER
UNIFORM_BLOCK_REFERENCED_BY_VERTEX_SHADER UNIFORM_BUFFER UNIFORM_BUFFER_BINDING
UNIFORM_BUFFER_OFFSET_ALIGNMENT UNIFORM_BUFFER_SIZE UNIFORM_BUFFER_START UNIFORM_IS_ROW_MAJOR
UNIFORM_MATRIX_STRIDE UNIFORM_OFFSET UNIFORM_SIZE UNIFORM_TYPE UNPACK_IMAGE_HEIGHT UNPACK_ROW_LENGTH
UNPACK_SKIP_IMAGES UNPACK_SKIP_PIXELS UNPACK_SKIP_ROWS UNSIGNALED UNSIGNED_INT_10F_11F_11F_REV
UNSIGNED_INT_24_8 UNSIGNED_INT_2_10_10_10_REV UNSIGNED_INT_5_9_9_9_REV UNSIGNED_INT_SAMPLER_2D
UNSIGNED_INT_SAMPLER_2D_ARRAY UNSIGNED_INT_SAMPLER_3D UNSIGNED_INT_SAMPLER_CUBE UNSIGNED_INT_VEC2
UNSIGNED_INT_VEC3 UNSIGNED_INT_VEC4 UNSIGNED_NORMALIZED VERTEX_ARRAY_BINDING
VERTEX_ATTRIB_ARRAY_DIVISOR VERTEX_ATTRIB_ARRAY_INTEGER WAIT_FAILED";

fn warn_once() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: WebGL preserves resources, shader/program state, queries and \
                 constants; GLSL translation and draw/upload execution on the native wgpu \
                 compositor are pending"
            );
        }
    });
}

fn illegal(name: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(&format!("Illegal constructor: {name}"))],
    ))
}

fn constant(name: &str) -> f64 {
    crate::webgl_constants_generated::value(name).unwrap_or(0.0)
}

fn object_value(name: &'static str) -> Value {
    let value = Value::object(HashMap::from([(
        "__w3cos_deleted".into(),
        Value::Bool(false),
    )]));
    w3cos_core::class::set_prototype_of(&value, &class_for(name).get_property("prototype"));
    register_weak_realm_object(&OBJECTS, &value);
    value
}

fn set_prototype(value: &Value, name: &'static str) {
    w3cos_core::class::set_prototype_of(value, &class_for(name).get_property("prototype"));
}

fn build_class(name: &'static str) -> Value {
    let constructor = realm_webgl_function(move |_, _| illegal(name));
    constructor.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
    if matches!(name, "WebGLRenderingContext" | "WebGL2RenderingContext") {
        let methods = if name == "WebGL2RenderingContext" {
            WEBGL2_METHODS
        } else {
            WEBGL1_METHODS
        };
        for method in methods.split_whitespace() {
            prototype.set_property(method, Value::Undefined);
        }
        for accessor in "canvas drawingBufferColorSpace drawingBufferFormat drawingBufferHeight drawingBufferWidth unpackColorSpace".split_whitespace() {
            prototype.set_property(accessor, Value::Undefined);
        }
        for constant_name in WEBGL1_CONSTANTS.split_whitespace().chain(
            (name == "WebGL2RenderingContext")
                .then_some(WEBGL2_EXTRA_CONSTANTS)
                .into_iter()
                .flat_map(str::split_whitespace),
        ) {
            let value = Value::Number(constant(constant_name));
            prototype.set_property(constant_name, value.clone());
            constructor.set_property(constant_name, value);
        }
    } else {
        for member in match name {
            "WebGLActiveInfo" => &["size", "type", "name"][..],
            "WebGLShaderPrecisionFormat" => &["rangeMin", "rangeMax", "precision"][..],
            _ => &[],
        } {
            prototype.set_property(member, Value::Undefined);
        }
        if name != "WebGLObject"
            && name != "WebGLUniformLocation"
            && name != "WebGLActiveInfo"
            && name != "WebGLShaderPrecisionFormat"
        {
            w3cos_core::class::set_prototype_of(
                &prototype,
                &class_for("WebGLObject").get_property("prototype"),
            );
        }
    }
    constructor.set_property("prototype", prototype);
    constructor
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn context_value(canvas: Value, webgl2: bool) -> Value {
    warn_once();
    let state = Rc::new(RefCell::new(HashMap::<String, Value>::from([
        (
            "clearColor".into(),
            Value::array(vec![Value::Number(0.0); 4]),
        ),
        ("viewport".into(), Value::array(vec![Value::Number(0.0); 4])),
    ])));
    let value = Value::object(HashMap::from([
        ("canvas".into(), canvas.clone()),
        ("drawingBufferColorSpace".into(), Value::string("srgb")),
        ("drawingBufferFormat".into(), Value::string("rgba8")),
        ("unpackColorSpace".into(), Value::string("srgb")),
    ]));
    let width_canvas = canvas.clone();
    value.set_property(
        "__w3cos_getter_drawingBufferWidth",
        realm_webgl_function(move |_, _| width_canvas.get_property("width")),
    );
    let height_canvas = canvas;
    value.set_property(
        "__w3cos_getter_drawingBufferHeight",
        realm_webgl_function(move |_, _| height_canvas.get_property("height")),
    );
    for (method, class_name) in [
        ("createBuffer", "WebGLBuffer"),
        ("createFramebuffer", "WebGLFramebuffer"),
        ("createProgram", "WebGLProgram"),
        ("createQuery", "WebGLQuery"),
        ("createRenderbuffer", "WebGLRenderbuffer"),
        ("createSampler", "WebGLSampler"),
        ("createShader", "WebGLShader"),
        ("createTexture", "WebGLTexture"),
        ("createTransformFeedback", "WebGLTransformFeedback"),
        ("createVertexArray", "WebGLVertexArrayObject"),
        ("fenceSync", "WebGLSync"),
        ("getUniformLocation", "WebGLUniformLocation"),
    ] {
        value.set_property(
            method,
            realm_webgl_function(move |_, _| object_value(class_name)),
        );
    }
    for (method, class_name) in [
        ("isBuffer", "WebGLBuffer"),
        ("isFramebuffer", "WebGLFramebuffer"),
        ("isProgram", "WebGLProgram"),
        ("isQuery", "WebGLQuery"),
        ("isRenderbuffer", "WebGLRenderbuffer"),
        ("isSampler", "WebGLSampler"),
        ("isShader", "WebGLShader"),
        ("isSync", "WebGLSync"),
        ("isTexture", "WebGLTexture"),
        ("isTransformFeedback", "WebGLTransformFeedback"),
        ("isVertexArray", "WebGLVertexArrayObject"),
    ] {
        value.set_property(
            method,
            realm_webgl_function(move |_, args| {
                let item = args.first().cloned().unwrap_or(Value::Undefined);
                Value::Bool(
                    w3cos_core::class::instance_of(&item, &class_for(class_name))
                        && !item.get_property("__w3cos_deleted").to_bool(),
                )
            }),
        );
    }
    for method in [
        "deleteBuffer",
        "deleteFramebuffer",
        "deleteProgram",
        "deleteQuery",
        "deleteRenderbuffer",
        "deleteSampler",
        "deleteShader",
        "deleteSync",
        "deleteTexture",
        "deleteTransformFeedback",
        "deleteVertexArray",
    ] {
        value.set_property(
            method,
            realm_webgl_function(|_, args| {
                if let Some(item) = args.first() {
                    item.set_property("__w3cos_deleted", Value::Bool(true));
                }
                Value::Undefined
            }),
        );
    }
    let state_for_clear = state.clone();
    value.set_property(
        "clearColor",
        realm_webgl_function(move |_, args| {
            state_for_clear.borrow_mut().insert(
                "clearColor".into(),
                Value::array(
                    (0..4)
                        .map(|index| args.get(index).cloned().unwrap_or(Value::Number(0.0)))
                        .collect(),
                ),
            );
            Value::Undefined
        }),
    );
    let state_for_viewport = state.clone();
    value.set_property(
        "viewport",
        realm_webgl_function(move |_, args| {
            state_for_viewport.borrow_mut().insert(
                "viewport".into(),
                Value::array(
                    (0..4)
                        .map(|index| args.get(index).cloned().unwrap_or(Value::Number(0.0)))
                        .collect(),
                ),
            );
            Value::Undefined
        }),
    );
    let state_for_parameter = state.clone();
    value.set_property(
        "getParameter",
        realm_webgl_function(
            move |_, args| match args.first().map(Value::to_u32).unwrap_or(0) {
                3106 => state_for_parameter.borrow()["clearColor"].clone(),
                2978 => state_for_parameter.borrow()["viewport"].clone(),
                7936 => Value::string("w3cos"),
                7937 => Value::string("w3cos wgpu compatibility renderer"),
                7938 => Value::string(if webgl2 { "WebGL 2.0" } else { "WebGL 1.0" }),
                35724 => Value::string(if webgl2 {
                    "WebGL GLSL ES 3.00"
                } else {
                    "WebGL GLSL ES 1.00"
                }),
                _ => Value::Null,
            },
        ),
    );
    value.set_property("getError", realm_webgl_function(|_, _| Value::Number(0.0)));
    value.set_property(
        "isContextLost",
        realm_webgl_function(|_, _| Value::Bool(false)),
    );
    value.set_property("isEnabled", realm_webgl_function(|_, _| Value::Bool(false)));
    value.set_property(
        "getSupportedExtensions",
        realm_webgl_function(|_, _| Value::array(Vec::new())),
    );
    value.set_property("getExtension", realm_webgl_function(|_, _| Value::Null));
    value.set_property(
        "getContextAttributes",
        realm_webgl_function(|_, _| {
            Value::object(HashMap::from([
                ("alpha".into(), Value::Bool(true)),
                ("antialias".into(), Value::Bool(false)),
                ("depth".into(), Value::Bool(true)),
                ("stencil".into(), Value::Bool(false)),
                ("premultipliedAlpha".into(), Value::Bool(true)),
                ("preserveDrawingBuffer".into(), Value::Bool(false)),
            ]))
        }),
    );
    value.set_property(
        "checkFramebufferStatus",
        realm_webgl_function(|_, _| Value::Number(36053.0)),
    );
    value.set_property(
        "getAttribLocation",
        realm_webgl_function(|_, _| Value::Number(0.0)),
    );
    value.set_property(
        "getProgramInfoLog",
        realm_webgl_function(|_, _| Value::string("")),
    );
    value.set_property(
        "getShaderInfoLog",
        realm_webgl_function(|_, _| Value::string("")),
    );
    value.set_property(
        "getShaderPrecisionFormat",
        realm_webgl_function(|_, _| {
            let result = Value::object(HashMap::from([
                ("rangeMin".into(), Value::Number(127.0)),
                ("rangeMax".into(), Value::Number(127.0)),
                ("precision".into(), Value::Number(23.0)),
            ]));
            w3cos_core::class::set_prototype_of(
                &result,
                &class_for("WebGLShaderPrecisionFormat").get_property("prototype"),
            );
            register_weak_realm_object(&OBJECTS, &result);
            result
        }),
    );
    value.set_property(
        "shaderSource",
        realm_webgl_function(|_, args| {
            if let Some(shader) = args.first() {
                shader.set_property(
                    "__w3cos_source",
                    Value::string(&args.get(1).map(Value::to_js_string).unwrap_or_default()),
                );
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "getShaderSource",
        realm_webgl_function(|_, args| {
            args.first()
                .map(|shader| shader.get_property("__w3cos_source"))
                .unwrap_or(Value::Null)
        }),
    );
    value.set_property(
        "getShaderParameter",
        realm_webgl_function(|_, _| Value::Bool(true)),
    );
    value.set_property(
        "getProgramParameter",
        realm_webgl_function(|_, _| Value::Bool(true)),
    );
    value.set_property(
        "makeXRCompatible",
        realm_webgl_function(|_, _| {
            warn_once();
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    let all_methods = if webgl2 {
        WEBGL2_METHODS
    } else {
        WEBGL1_METHODS
    };
    for method in all_methods.split_whitespace() {
        if value.get_property(method).is_undefined() {
            value.set_property(
                method,
                realm_webgl_function(move |_, _| {
                    if matches!(
                        method,
                        "drawArrays"
                            | "drawElements"
                            | "drawArraysInstanced"
                            | "drawElementsInstanced"
                    ) {
                        warn_once();
                    }
                    Value::Undefined
                }),
            );
        }
    }
    for name in WEBGL1_CONSTANTS.split_whitespace().chain(
        webgl2
            .then_some(WEBGL2_EXTRA_CONSTANTS)
            .into_iter()
            .flat_map(str::split_whitespace),
    ) {
        value.set_property(name, Value::Number(constant(name)));
    }
    set_prototype(
        &value,
        if webgl2 {
            "WebGL2RenderingContext"
        } else {
            "WebGLRenderingContext"
        },
    );
    register_weak_realm_object(&CONTEXTS, &value);
    value
}

pub const INTERFACES: &[&str] = &[
    "WebGL2RenderingContext",
    "WebGLActiveInfo",
    "WebGLBuffer",
    "WebGLFramebuffer",
    "WebGLObject",
    "WebGLProgram",
    "WebGLQuery",
    "WebGLRenderbuffer",
    "WebGLRenderingContext",
    "WebGLSampler",
    "WebGLShader",
    "WebGLShaderPrecisionFormat",
    "WebGLSync",
    "WebGLTexture",
    "WebGLTransformFeedback",
    "WebGLUniformLocation",
    "WebGLVertexArrayObject",
];

pub fn reset() {
    CONTEXTS.with(|contexts| {
        for context in contexts
            .borrow_mut()
            .drain(..)
            .filter_map(|context| upgrade_realm_object(&context))
        {
            context.set_property("canvas", Value::Undefined);
            context.set_property("__w3cos_getter_drawingBufferHeight", Value::Undefined);
            context.set_property("__w3cos_getter_drawingBufferWidth", Value::Undefined);
            for method in WEBGL1_METHODS
                .split_whitespace()
                .chain(WEBGL2_METHODS.split_whitespace())
            {
                context.set_property(method, Value::Undefined);
            }
        }
    });
    OBJECTS.with(|objects| {
        for object in objects
            .borrow_mut()
            .drain(..)
            .filter_map(|object| upgrade_realm_object(&object))
        {
            object.set_property("__w3cos_deleted", Value::Bool(true));
            object.set_property("__w3cos_source", Value::Undefined);
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        disconnect_realm_class(class);
    }
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_tracks_resources_shader_source_and_query_state() {
        reset();
        let canvas = Value::object(HashMap::from([
            ("width".into(), Value::Number(640.0)),
            ("height".into(), Value::Number(480.0)),
        ]));
        let gl = context_value(canvas, false);
        let buffer = gl.call_method("createBuffer", vec![]);
        assert!(gl.call_method("isBuffer", vec![buffer.clone()]).to_bool());
        gl.call_method("deleteBuffer", vec![buffer.clone()]);
        assert!(!gl.call_method("isBuffer", vec![buffer]).to_bool());
        let shader = gl.call_method("createShader", vec![Value::Number(35633.0)]);
        gl.call_method(
            "shaderSource",
            vec![shader.clone(), Value::string("void main(){}")],
        );
        assert_eq!(
            gl.call_method("getShaderSource", vec![shader])
                .to_js_string(),
            "void main(){}"
        );
        gl.call_method(
            "clearColor",
            vec![
                Value::Number(1.0),
                Value::Number(0.5),
                Value::Number(0.0),
                Value::Number(1.0),
            ],
        );
        assert_eq!(
            gl.call_method("getParameter", vec![Value::Number(3106.0)])
                .get_property("1")
                .to_number(),
            0.5
        );
    }

    #[test]
    fn webgl2_exposes_current_constants_and_resource_types() {
        reset();
        let gl = context_value(Value::object(HashMap::new()), true);
        assert_eq!(gl.get_property("ARRAY_BUFFER").to_number(), 34962.0);
        assert_eq!(gl.get_property("TEXTURE31").to_number(), 34015.0);
        assert!(gl.call_method("createVertexArray", vec![]).is_object());
    }

    #[test]
    fn contexts_resources_canvas_references_and_classes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_context_class = class_for("WebGLRenderingContext");
        let old_shader_class = class_for("WebGLShader");
        let canvas = Value::object(HashMap::from([
            ("width".into(), Value::Number(320.0)),
            ("height".into(), Value::Number(200.0)),
        ]));
        let canvas_weak = crate::jsdom::weak_realm_object(&canvas);
        let gl = context_value(canvas.clone(), false);
        drop(canvas);
        let shader = gl.call_method("createShader", vec![Value::Number(35633.0)]);
        gl.call_method(
            "shaderSource",
            vec![shader.clone(), Value::string("void main(){}")],
        );
        let gl_weak = crate::jsdom::weak_realm_object(&gl);
        let shader_weak = crate::jsdom::weak_realm_object(&shader);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_context_class.get_property("prototype").is_undefined());
        assert!(old_shader_class.get_property("prototype").is_undefined());
        assert!(!old_context_class.strict_eq(&class_for("WebGLRenderingContext")));
        assert!(gl.get_property("canvas").is_undefined());
        assert!(gl.get_property("drawingBufferWidth").is_undefined());
        assert!(gl.call_method("createBuffer", Vec::new()).is_undefined());
        assert!(gl.call_method("getError", Vec::new()).is_undefined());
        assert!(shader.get_property("__w3cos_deleted").is_undefined());
        assert!(shader.get_property("__w3cos_source").is_undefined());
        assert!(canvas_weak.upgrade().is_none());

        drop(gl);
        drop(shader);
        assert!(gl_weak.upgrade().is_none());
        assert!(shader_weak.upgrade().is_none());
    }
}
