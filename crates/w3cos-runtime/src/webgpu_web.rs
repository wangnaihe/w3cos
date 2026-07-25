//! WebGPU facade backed by the same native `wgpu` stack used by Vello.
//!
//! Adapter/device discovery, buffers, queue writes, shader modules, and basic
//! command submission use native GPU objects. Higher-level pipeline
//! descriptor translation remains an explicit compatibility boundary.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

#[cfg(feature = "gpu")]
use vello::wgpu;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static GPU_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

#[cfg(feature = "gpu")]
struct DeviceState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    limits: wgpu::Limits,
}

#[cfg(feature = "gpu")]
struct BufferState {
    buffer: wgpu::Buffer,
    size: u64,
    usage: u32,
    mapped: RefCell<Option<Value>>,
    mapped_at_creation: Cell<bool>,
    destroyed: Cell<bool>,
}

#[cfg(feature = "gpu")]
thread_local! {
    static BUFFERS: RefCell<Vec<Rc<BufferState>>> = const { RefCell::new(Vec::new()) };
    static COMMAND_ENCODERS: RefCell<Vec<Rc<RefCell<Option<wgpu::CommandEncoder>>>>> =
        const { RefCell::new(Vec::new()) };
    static COMMAND_BUFFERS: RefCell<Vec<Rc<RefCell<Option<wgpu::CommandBuffer>>>>> =
        const { RefCell::new(Vec::new()) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn error(name: &str, message: &str) -> Value {
    if matches!(name, "TypeError" | "RangeError") {
        w3cos_core::error_instance(name, vec![Value::string(message)])
    } else {
        w3cos_core::web::dom_exception_instance(message, name)
    }
}

fn throw(name: &str, message: &str) -> ! {
    w3cos_core::throw_value(error(name, message))
}

fn warn_once() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: WebGPU uses the runtime's native wgpu adapter for devices, \
                 buffers, queue writes, shaders and basic command submission; full texture, \
                 bind-group, pipeline, render/compute-pass and canvas presentation translation \
                 remains pending"
            );
        }
    });
}

fn unavailable(api: &str) -> ! {
    warn_once();
    throw(
        "NotSupportedError",
        &format!("{api} descriptor translation is not implemented"),
    )
}

fn set_prototype(value: &Value, name: &'static str) {
    w3cos_core::class::set_prototype_of(value, &class_for(name).get_property("prototype"));
}

fn empty_set_like(name: &'static str) -> Value {
    let value = Value::object(HashMap::new());
    value.set_property(
        "__w3cos_getter_size",
        Value::function(|_, _| Value::Number(0.0)),
    );
    value.set_property("has", Value::function(|_, _| Value::Bool(false)));
    value.set_property("forEach", Value::function(|_, _| Value::Undefined));
    for operation in ["entries", "keys", "values"] {
        value.set_property(operation, Value::function(|_, _| Value::array(Vec::new())));
    }
    value.set_property("__w3cos_symbol_iterator", value.get_property("values"));
    set_prototype(&value, name);
    value
}

fn limits_value() -> Value {
    let value = Value::object(HashMap::new());
    for (name, number) in [
        ("maxBindGroups", 4.0),
        ("maxBindGroupsPlusVertexBuffers", 24.0),
        ("maxBindingsPerBindGroup", 1000.0),
        ("maxBufferSize", 268_435_456.0),
        ("maxColorAttachmentBytesPerSample", 32.0),
        ("maxColorAttachments", 8.0),
        ("maxComputeInvocationsPerWorkgroup", 256.0),
        ("maxComputeWorkgroupSizeX", 256.0),
        ("maxComputeWorkgroupSizeY", 256.0),
        ("maxComputeWorkgroupSizeZ", 64.0),
        ("maxComputeWorkgroupStorageSize", 16_384.0),
        ("maxComputeWorkgroupsPerDimension", 65_535.0),
        ("maxDynamicStorageBuffersPerPipelineLayout", 4.0),
        ("maxDynamicUniformBuffersPerPipelineLayout", 8.0),
        ("maxImmediateSize", 0.0),
        ("maxInterStageShaderVariables", 16.0),
        ("maxSampledTexturesPerShaderStage", 16.0),
        ("maxSamplersPerShaderStage", 16.0),
        ("maxStorageBufferBindingSize", 134_217_728.0),
        ("maxStorageBuffersInFragmentStage", 8.0),
        ("maxStorageBuffersInVertexStage", 8.0),
        ("maxStorageBuffersPerShaderStage", 8.0),
        ("maxStorageTexturesInFragmentStage", 4.0),
        ("maxStorageTexturesInVertexStage", 4.0),
        ("maxStorageTexturesPerShaderStage", 4.0),
        ("maxTextureArrayLayers", 256.0),
        ("maxTextureDimension1D", 8192.0),
        ("maxTextureDimension2D", 8192.0),
        ("maxTextureDimension3D", 2048.0),
        ("maxUniformBufferBindingSize", 65_536.0),
        ("maxUniformBuffersPerShaderStage", 12.0),
        ("maxVertexAttributes", 16.0),
        ("maxVertexBufferArrayStride", 2048.0),
        ("maxVertexBuffers", 8.0),
        ("minStorageBufferOffsetAlignment", 256.0),
        ("minUniformBufferOffsetAlignment", 256.0),
    ] {
        value.set_property(name, Value::Number(number));
    }
    set_prototype(&value, "GPUSupportedLimits");
    value
}

#[cfg(feature = "gpu")]
fn adapter_info_value(info: &wgpu::AdapterInfo) -> Value {
    let value = Value::object(HashMap::from([
        (
            "vendor".into(),
            Value::string(&format!("{:#x}", info.vendor)),
        ),
        ("architecture".into(), Value::string("")),
        (
            "device".into(),
            Value::string(&format!("{:#x}", info.device)),
        ),
        ("description".into(), Value::string(&info.name)),
        ("subgroupMinSize".into(), Value::Number(0.0)),
        ("subgroupMaxSize".into(), Value::Number(0.0)),
        ("isFallbackAdapter".into(), Value::Bool(false)),
    ]));
    set_prototype(&value, "GPUAdapterInfo");
    value
}

#[cfg(feature = "gpu")]
fn buffer_id(value: &Value) -> Option<usize> {
    let id = value.get_property("__w3cos_buffer_id").to_number();
    id.is_finite().then_some(id as usize)
}

#[cfg(feature = "gpu")]
fn buffer_state(value: &Value) -> Option<Rc<BufferState>> {
    let id = buffer_id(value)?;
    BUFFERS.with(|buffers| buffers.borrow().get(id).cloned())
}

#[cfg(feature = "gpu")]
fn buffer_value(state: BufferState, label: String) -> Value {
    let state = Rc::new(state);
    let id = BUFFERS.with(|buffers| {
        let mut buffers = buffers.borrow_mut();
        let id = buffers.len();
        buffers.push(state.clone());
        id
    });
    let value = Value::object(HashMap::from([
        ("__w3cos_buffer_id".into(), Value::Number(id as f64)),
        ("label".into(), Value::string(&label)),
        ("size".into(), Value::Number(state.size as f64)),
        ("usage".into(), Value::Number(state.usage as f64)),
    ]));
    let state_for_range = state.clone();
    value.set_property(
        "getMappedRange",
        Value::function(move |_, _| {
            if state_for_range.destroyed.get() || !state_for_range.mapped_at_creation.get() {
                throw("InvalidStateError", "GPUBuffer is not mapped");
            }
            if let Some(value) = state_for_range.mapped.borrow().clone() {
                return value;
            }
            let bytes = vec![0; state_for_range.size as usize];
            let value = w3cos_core::binary::array_buffer_value(bytes);
            *state_for_range.mapped.borrow_mut() = Some(value.clone());
            value
        }),
    );
    let state_for_unmap = state.clone();
    value.set_property(
        "unmap",
        Value::function(move |_, _| {
            if state_for_unmap.mapped_at_creation.replace(false) {
                if let Some(mapped) = state_for_unmap.mapped.borrow_mut().take()
                    && let Some(bytes) = w3cos_core::binary::bytes_of(&mapped)
                {
                    let mut range = state_for_unmap.buffer.slice(..).get_mapped_range_mut();
                    let length = range.len().min(bytes.len());
                    range[..length].copy_from_slice(&bytes[..length]);
                    drop(range);
                }
                state_for_unmap.buffer.unmap();
            }
            Value::Undefined
        }),
    );
    let state_for_destroy = state.clone();
    value.set_property(
        "destroy",
        Value::function(move |_, _| {
            if !state_for_destroy.destroyed.replace(true) {
                state_for_destroy.buffer.destroy();
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "mapAsync",
        Value::function(|_, _| {
            warn_once();
            w3cos_core::promise::reject(vec![error(
                "NotSupportedError",
                "GPUBuffer.mapAsync requires asynchronous device polling integration",
            )])
        }),
    );
    value.set_property("__w3cos_getter_mapState", {
        let state = state.clone();
        Value::function(move |_, _| {
            Value::string(if state.mapped_at_creation.get() {
                "mapped"
            } else {
                "unmapped"
            })
        })
    });
    set_prototype(&value, "GPUBuffer");
    value
}

#[cfg(feature = "gpu")]
fn create_buffer(state: Rc<DeviceState>, descriptor: Value) -> Value {
    let size = descriptor.get_property("size").to_number();
    if !size.is_finite() || size <= 0.0 {
        throw("RangeError", "GPUBufferDescriptor.size must be positive");
    }
    let usage = descriptor.get_property("usage").to_u32();
    if usage == 0 {
        throw("TypeError", "GPUBufferDescriptor.usage must not be zero");
    }
    let mapped = descriptor.get_property("mappedAtCreation").to_bool();
    let label = descriptor.get_property("label").to_js_string();
    let buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
        label: (!label.is_empty()).then_some(label.as_str()),
        size: size as u64,
        usage: wgpu::BufferUsages::from_bits_truncate(usage),
        mapped_at_creation: mapped,
    });
    buffer_value(
        BufferState {
            buffer,
            size: size as u64,
            usage,
            mapped: RefCell::new(None),
            mapped_at_creation: Cell::new(mapped),
            destroyed: Cell::new(false),
        },
        label,
    )
}

#[cfg(feature = "gpu")]
fn queue_value(state: Rc<DeviceState>) -> Value {
    let value = Value::object(HashMap::from([("label".into(), Value::string(""))]));
    let state_for_write = state.clone();
    value.set_property(
        "writeBuffer",
        Value::function(move |_, args| {
            let destination = arg(&args, 0);
            let Some(buffer) = buffer_state(&destination) else {
                throw(
                    "TypeError",
                    "GPUQueue.writeBuffer destination must be a GPUBuffer",
                );
            };
            let offset = arg(&args, 1).to_number().max(0.0) as u64;
            let Some(bytes) = w3cos_core::binary::bytes_of(&arg(&args, 2)) else {
                throw(
                    "TypeError",
                    "GPUQueue.writeBuffer data must be a BufferSource",
                );
            };
            state_for_write
                .queue
                .write_buffer(&buffer.buffer, offset, &bytes);
            Value::Undefined
        }),
    );
    let state_for_submit = state.clone();
    value.set_property(
        "submit",
        Value::function(move |_, args| {
            let mut command_buffers = Vec::new();
            for item in arg(&args, 0).iter() {
                let id = item.get_property("__w3cos_command_buffer_id").to_number() as usize;
                COMMAND_BUFFERS.with(|buffers| {
                    if let Some(buffer) = buffers.borrow().get(id)
                        && let Some(buffer) = buffer.borrow_mut().take()
                    {
                        command_buffers.push(buffer);
                    }
                });
            }
            state_for_submit.queue.submit(command_buffers);
            Value::Undefined
        }),
    );
    value.set_property(
        "onSubmittedWorkDone",
        Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
    );
    for operation in ["copyExternalImageToTexture", "writeTexture"] {
        value.set_property(
            operation,
            Value::function(move |_, _| unavailable(&format!("GPUQueue.{operation}"))),
        );
    }
    set_prototype(&value, "GPUQueue");
    value
}

#[cfg(feature = "gpu")]
fn command_encoder_value(state: Rc<DeviceState>, descriptor: Value) -> Value {
    let label = descriptor.get_property("label").to_js_string();
    let encoder = state
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: (!label.is_empty()).then_some(label.as_str()),
        });
    let encoder = Rc::new(RefCell::new(Some(encoder)));
    let id = COMMAND_ENCODERS.with(|encoders| {
        let mut encoders = encoders.borrow_mut();
        let id = encoders.len();
        encoders.push(encoder.clone());
        id
    });
    let value = Value::object(HashMap::from([
        ("label".into(), Value::string(&label)),
        (
            "__w3cos_command_encoder_id".into(),
            Value::Number(id as f64),
        ),
    ]));
    let encoder_for_copy = encoder.clone();
    value.set_property(
        "copyBufferToBuffer",
        Value::function(move |_, args| {
            let Some(source) = buffer_state(&arg(&args, 0)) else {
                throw("TypeError", "copyBufferToBuffer source must be a GPUBuffer");
            };
            let source_offset = arg(&args, 1).to_number().max(0.0) as u64;
            let Some(destination) = buffer_state(&arg(&args, 2)) else {
                throw(
                    "TypeError",
                    "copyBufferToBuffer destination must be a GPUBuffer",
                );
            };
            let destination_offset = arg(&args, 3).to_number().max(0.0) as u64;
            let size = arg(&args, 4).to_number().max(0.0) as u64;
            let mut encoder = encoder_for_copy.borrow_mut();
            let Some(encoder) = encoder.as_mut() else {
                throw("InvalidStateError", "GPUCommandEncoder is already finished");
            };
            encoder.copy_buffer_to_buffer(
                &source.buffer,
                source_offset,
                &destination.buffer,
                destination_offset,
                size,
            );
            Value::Undefined
        }),
    );
    let encoder_for_clear = encoder.clone();
    value.set_property(
        "clearBuffer",
        Value::function(move |_, args| {
            let Some(buffer) = buffer_state(&arg(&args, 0)) else {
                throw("TypeError", "clearBuffer target must be a GPUBuffer");
            };
            let offset = arg(&args, 1).to_number().max(0.0) as u64;
            let size = arg(&args, 2);
            let mut encoder = encoder_for_clear.borrow_mut();
            let Some(encoder) = encoder.as_mut() else {
                throw("InvalidStateError", "GPUCommandEncoder is already finished");
            };
            encoder.clear_buffer(
                &buffer.buffer,
                offset,
                (!size.is_undefined()).then(|| size.to_number().max(0.0) as u64),
            );
            Value::Undefined
        }),
    );
    let encoder_for_finish = encoder;
    value.set_property(
        "finish",
        Value::function(move |_, args| {
            let mut encoder = encoder_for_finish.borrow_mut();
            let Some(encoder) = encoder.take() else {
                throw("InvalidStateError", "GPUCommandEncoder is already finished");
            };
            let command_buffer = encoder.finish();
            let command_buffer = Rc::new(RefCell::new(Some(command_buffer)));
            let id = COMMAND_BUFFERS.with(|buffers| {
                let mut buffers = buffers.borrow_mut();
                let id = buffers.len();
                buffers.push(command_buffer);
                id
            });
            let descriptor = arg(&args, 0);
            let result = Value::object(HashMap::from([
                (
                    "label".into(),
                    Value::string(&descriptor.get_property("label").to_js_string()),
                ),
                ("__w3cos_command_buffer_id".into(), Value::Number(id as f64)),
            ]));
            set_prototype(&result, "GPUCommandBuffer");
            result
        }),
    );
    for operation in [
        "beginComputePass",
        "beginRenderPass",
        "copyBufferToTexture",
        "copyTextureToBuffer",
        "copyTextureToTexture",
        "insertDebugMarker",
        "popDebugGroup",
        "pushDebugGroup",
        "resolveQuerySet",
    ] {
        if value.get_property(operation).is_undefined() {
            value.set_property(
                operation,
                Value::function(move |_, _| unavailable(&format!("GPUCommandEncoder.{operation}"))),
            );
        }
    }
    set_prototype(&value, "GPUCommandEncoder");
    value
}

#[cfg(feature = "gpu")]
fn shader_module_value(state: Rc<DeviceState>, descriptor: Value) -> Value {
    let code = descriptor.get_property("code").to_js_string();
    if code.is_empty() {
        throw("TypeError", "GPUShaderModuleDescriptor.code is required");
    }
    let label = descriptor.get_property("label").to_js_string();
    let module = Rc::new(
        state
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: (!label.is_empty()).then_some(label.as_str()),
                source: wgpu::ShaderSource::Wgsl(code.into()),
            }),
    );
    let value = Value::object(HashMap::from([("label".into(), Value::string(&label))]));
    value.set_property(
        "__w3cos_keepalive",
        Value::function(move |_, _| {
            let _ = &module;
            Value::Undefined
        }),
    );
    value.set_property(
        "getCompilationInfo",
        Value::function(|_, _| {
            warn_once();
            let info = Value::object(HashMap::from([(
                "messages".into(),
                Value::array(Vec::new()),
            )]));
            set_prototype(&info, "GPUCompilationInfo");
            w3cos_core::promise::resolve(vec![info])
        }),
    );
    set_prototype(&value, "GPUShaderModule");
    value
}

#[cfg(feature = "gpu")]
fn device_value(adapter: Rc<wgpu::Adapter>) -> Value {
    let descriptor = wgpu::DeviceDescriptor {
        label: None,
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        ..Default::default()
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&descriptor)) else {
        return Value::Null;
    };
    let state = Rc::new(DeviceState {
        limits: adapter.limits(),
        info: adapter.get_info(),
        device,
        queue,
    });
    let value = Value::object(HashMap::from([
        ("label".into(), Value::string("")),
        ("features".into(), empty_set_like("GPUSupportedFeatures")),
        ("limits".into(), limits_value()),
        ("adapterInfo".into(), adapter_info_value(&state.info)),
        ("queue".into(), queue_value(state.clone())),
        (
            "lost".into(),
            w3cos_core::promise::new(vec![Value::function(|_, _| Value::Undefined)]),
        ),
        ("onuncapturederror".into(), Value::Null),
    ]));
    let state_for_buffer = state.clone();
    value.set_property(
        "createBuffer",
        Value::function(move |_, args| create_buffer(state_for_buffer.clone(), arg(&args, 0))),
    );
    let state_for_encoder = state.clone();
    value.set_property(
        "createCommandEncoder",
        Value::function(move |_, args| {
            command_encoder_value(state_for_encoder.clone(), arg(&args, 0))
        }),
    );
    let state_for_shader = state.clone();
    value.set_property(
        "createShaderModule",
        Value::function(move |_, args| {
            shader_module_value(state_for_shader.clone(), arg(&args, 0))
        }),
    );
    let state_for_destroy = state.clone();
    value.set_property(
        "destroy",
        Value::function(move |_, _| {
            state_for_destroy.device.destroy();
            Value::Undefined
        }),
    );
    value.set_property("pushErrorScope", Value::function(|_, _| Value::Undefined));
    value.set_property(
        "popErrorScope",
        Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Null])),
    );
    for operation in [
        "createBindGroup",
        "createBindGroupLayout",
        "createComputePipeline",
        "createPipelineLayout",
        "createQuerySet",
        "createRenderBundleEncoder",
        "createRenderPipeline",
        "createSampler",
        "createTexture",
        "importExternalTexture",
    ] {
        value.set_property(
            operation,
            Value::function(move |_, _| unavailable(&format!("GPUDevice.{operation}"))),
        );
    }
    for operation in ["createComputePipelineAsync", "createRenderPipelineAsync"] {
        value.set_property(
            operation,
            Value::function(move |_, _| {
                warn_once();
                w3cos_core::promise::reject(vec![error(
                    "NotSupportedError",
                    &format!("GPUDevice.{operation} descriptor translation is not implemented"),
                )])
            }),
        );
    }
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    set_prototype(&value, "GPUDevice");
    value
}

#[cfg(feature = "gpu")]
fn adapter_value(adapter: wgpu::Adapter) -> Value {
    let adapter = Rc::new(adapter);
    let info = adapter.get_info();
    let value = Value::object(HashMap::from([
        ("features".into(), empty_set_like("GPUSupportedFeatures")),
        ("limits".into(), limits_value()),
        ("info".into(), adapter_info_value(&info)),
    ]));
    value.set_property("requestDevice", {
        let adapter = adapter.clone();
        Value::function(move |_, _| {
            let device = device_value(adapter.clone());
            if device.is_null() {
                warn_once();
                w3cos_core::promise::reject(vec![error(
                    "OperationError",
                    "native WebGPU adapter could not create a device",
                )])
            } else {
                w3cos_core::promise::resolve(vec![device])
            }
        })
    });
    set_prototype(&value, "GPUAdapter");
    value
}

#[cfg(feature = "gpu")]
fn request_adapter() -> Value {
    let backends = wgpu::Backends::from_env().unwrap_or_default();
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
    });
    match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(adapter) => w3cos_core::promise::resolve(vec![adapter_value(adapter)]),
        Err(_) => {
            warn_once();
            w3cos_core::promise::resolve(vec![Value::Null])
        }
    }
}

#[cfg(not(feature = "gpu"))]
fn request_adapter() -> Value {
    warn_once();
    w3cos_core::promise::resolve(vec![Value::Null])
}

pub fn gpu_value() -> Value {
    GPU_VALUE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::from([
            (
                "requestAdapter".into(),
                Value::function(|_, _| request_adapter()),
            ),
            (
                "getPreferredCanvasFormat".into(),
                Value::function(|_, _| Value::string("bgra8unorm")),
            ),
            (
                "wgslLanguageFeatures".into(),
                crate::experimental_web::wgsl_language_features_value(),
            ),
        ]));
        set_prototype(&value, "GPU");
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn members(name: &str) -> &'static [&'static str] {
    match name {
        "GPU" => &[
            "getPreferredCanvasFormat",
            "requestAdapter",
            "wgslLanguageFeatures",
        ],
        "GPUAdapter" => &["features", "info", "limits", "requestDevice"],
        "GPUAdapterInfo" => &[
            "architecture",
            "description",
            "device",
            "isFallbackAdapter",
            "subgroupMaxSize",
            "subgroupMinSize",
            "vendor",
        ],
        "GPUBindGroup" | "GPUBindGroupLayout" | "GPUCommandBuffer" | "GPUExternalTexture"
        | "GPUPipelineLayout" | "GPURenderBundle" | "GPUSampler" | "GPUTextureView" => &["label"],
        "GPUBuffer" => &[
            "destroy",
            "getMappedRange",
            "label",
            "mapAsync",
            "mapState",
            "size",
            "unmap",
            "usage",
        ],
        "GPUCanvasContext" => &[
            "canvas",
            "configure",
            "getConfiguration",
            "getCurrentTexture",
            "unconfigure",
        ],
        "GPUCommandEncoder" => &[
            "beginComputePass",
            "beginRenderPass",
            "clearBuffer",
            "copyBufferToBuffer",
            "copyBufferToTexture",
            "copyTextureToBuffer",
            "copyTextureToTexture",
            "finish",
            "insertDebugMarker",
            "label",
            "popDebugGroup",
            "pushDebugGroup",
            "resolveQuerySet",
        ],
        "GPUCompilationInfo" => &["messages"],
        "GPUCompilationMessage" => &["length", "lineNum", "linePos", "message", "offset", "type"],
        "GPUComputePassEncoder" => &[
            "dispatchWorkgroups",
            "dispatchWorkgroupsIndirect",
            "end",
            "insertDebugMarker",
            "label",
            "popDebugGroup",
            "pushDebugGroup",
            "setBindGroup",
            "setImmediates",
            "setPipeline",
            "writeTimestamp",
        ],
        "GPUComputePipeline" | "GPURenderPipeline" => &["getBindGroupLayout", "label"],
        "GPUDevice" => &[
            "adapterInfo",
            "createBindGroup",
            "createBindGroupLayout",
            "createBuffer",
            "createCommandEncoder",
            "createComputePipeline",
            "createComputePipelineAsync",
            "createPipelineLayout",
            "createQuerySet",
            "createRenderBundleEncoder",
            "createRenderPipeline",
            "createRenderPipelineAsync",
            "createSampler",
            "createShaderModule",
            "createTexture",
            "destroy",
            "features",
            "importExternalTexture",
            "label",
            "limits",
            "lost",
            "onuncapturederror",
            "popErrorScope",
            "pushErrorScope",
            "queue",
        ],
        "GPUDeviceLostInfo" => &["message", "reason"],
        "GPUError" | "GPUInternalError" | "GPUOutOfMemoryError" | "GPUValidationError" => {
            &["message"]
        }
        "GPUPipelineError" => &["reason"],
        "GPUQuerySet" => &["count", "destroy", "label", "type"],
        "GPUQueue" => &[
            "copyExternalImageToTexture",
            "label",
            "onSubmittedWorkDone",
            "submit",
            "writeBuffer",
            "writeTexture",
        ],
        "GPURenderBundleEncoder" => &[
            "draw",
            "drawIndexed",
            "drawIndexedIndirect",
            "drawIndirect",
            "finish",
            "insertDebugMarker",
            "label",
            "popDebugGroup",
            "pushDebugGroup",
            "setBindGroup",
            "setImmediates",
            "setIndexBuffer",
            "setPipeline",
            "setVertexBuffer",
        ],
        "GPURenderPassEncoder" => &[
            "beginOcclusionQuery",
            "draw",
            "drawIndexed",
            "drawIndexedIndirect",
            "drawIndirect",
            "end",
            "endOcclusionQuery",
            "executeBundles",
            "insertDebugMarker",
            "label",
            "popDebugGroup",
            "pushDebugGroup",
            "setBindGroup",
            "setBlendConstant",
            "setImmediates",
            "setIndexBuffer",
            "setPipeline",
            "setScissorRect",
            "setStencilReference",
            "setVertexBuffer",
            "setViewport",
            "writeTimestamp",
        ],
        "GPUShaderModule" => &["getCompilationInfo", "label"],
        "GPUSupportedFeatures" => &["entries", "forEach", "has", "keys", "size", "values"],
        "GPUSupportedLimits" => &[
            "maxBindGroups",
            "maxBindGroupsPlusVertexBuffers",
            "maxBindingsPerBindGroup",
            "maxBufferSize",
            "maxColorAttachmentBytesPerSample",
            "maxColorAttachments",
            "maxComputeInvocationsPerWorkgroup",
            "maxComputeWorkgroupSizeX",
            "maxComputeWorkgroupSizeY",
            "maxComputeWorkgroupSizeZ",
            "maxComputeWorkgroupStorageSize",
            "maxComputeWorkgroupsPerDimension",
            "maxDynamicStorageBuffersPerPipelineLayout",
            "maxDynamicUniformBuffersPerPipelineLayout",
            "maxImmediateSize",
            "maxInterStageShaderVariables",
            "maxSampledTexturesPerShaderStage",
            "maxSamplersPerShaderStage",
            "maxStorageBufferBindingSize",
            "maxStorageBuffersInFragmentStage",
            "maxStorageBuffersInVertexStage",
            "maxStorageBuffersPerShaderStage",
            "maxStorageTexturesInFragmentStage",
            "maxStorageTexturesInVertexStage",
            "maxStorageTexturesPerShaderStage",
            "maxTextureArrayLayers",
            "maxTextureDimension1D",
            "maxTextureDimension2D",
            "maxTextureDimension3D",
            "maxUniformBufferBindingSize",
            "maxUniformBuffersPerShaderStage",
            "maxVertexAttributes",
            "maxVertexBufferArrayStride",
            "maxVertexBuffers",
            "minStorageBufferOffsetAlignment",
            "minUniformBufferOffsetAlignment",
        ],
        "GPUTexture" => &[
            "createView",
            "depthOrArrayLayers",
            "destroy",
            "dimension",
            "format",
            "height",
            "label",
            "mipLevelCount",
            "sampleCount",
            "textureBindingViewDimension",
            "usage",
            "width",
        ],
        _ => &[],
    }
}

fn constructible_error_class(name: &'static str) -> bool {
    matches!(
        name,
        "GPUError"
            | "GPUInternalError"
            | "GPUOutOfMemoryError"
            | "GPUValidationError"
            | "GPUPipelineError"
    )
}

fn build_class(name: &'static str) -> Value {
    let constructor = if constructible_error_class(name) {
        Value::function(move |_, args| {
            let message = arg(&args, 0).to_js_string();
            let value = w3cos_core::error_instance("Error", vec![Value::string(&message)]);
            value.set_property("message", Value::string(&message));
            if name == "GPUPipelineError" {
                value.set_property(
                    "reason",
                    Value::string(&arg(&args, 1).get_property("reason").to_js_string()),
                );
            }
            set_prototype(&value, name);
            value
        })
    } else {
        Value::function(move |_, _| throw("TypeError", &format!("Illegal constructor: {name}")))
    };
    constructor.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
    for member in members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    if name == "GPUDevice" {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
    } else if matches!(
        name,
        "GPUInternalError" | "GPUOutOfMemoryError" | "GPUValidationError"
    ) {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &class_for("GPUError").get_property("prototype"),
        );
    } else if name == "GPUError" || name == "GPUPipelineError" {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &w3cos_core::error_class("Error").get_property("prototype"),
        );
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

pub fn constant_value(name: &str) -> Value {
    let entries: &[(&str, u32)] = match name {
        "GPUBufferUsage" => &[
            ("MAP_READ", 1),
            ("MAP_WRITE", 2),
            ("COPY_SRC", 4),
            ("COPY_DST", 8),
            ("INDEX", 16),
            ("VERTEX", 32),
            ("UNIFORM", 64),
            ("STORAGE", 128),
            ("INDIRECT", 256),
            ("QUERY_RESOLVE", 512),
        ],
        "GPUColorWrite" => &[
            ("RED", 1),
            ("GREEN", 2),
            ("BLUE", 4),
            ("ALPHA", 8),
            ("ALL", 15),
        ],
        "GPUMapMode" => &[("READ", 1), ("WRITE", 2)],
        "GPUShaderStage" => &[("VERTEX", 1), ("FRAGMENT", 2), ("COMPUTE", 4)],
        "GPUTextureUsage" => &[
            ("COPY_SRC", 1),
            ("COPY_DST", 2),
            ("TEXTURE_BINDING", 4),
            ("STORAGE_BINDING", 8),
            ("RENDER_ATTACHMENT", 16),
            ("TRANSIENT_ATTACHMENT", 32),
        ],
        _ => &[],
    };
    Value::object(
        entries
            .iter()
            .map(|(key, value)| ((*key).into(), Value::Number(*value as f64)))
            .collect(),
    )
}

pub const INTERFACES: &[&str] = &[
    "GPU",
    "GPUAdapter",
    "GPUAdapterInfo",
    "GPUBindGroup",
    "GPUBindGroupLayout",
    "GPUBuffer",
    "GPUCanvasContext",
    "GPUCommandBuffer",
    "GPUCommandEncoder",
    "GPUCompilationInfo",
    "GPUCompilationMessage",
    "GPUComputePassEncoder",
    "GPUComputePipeline",
    "GPUDevice",
    "GPUDeviceLostInfo",
    "GPUError",
    "GPUExternalTexture",
    "GPUInternalError",
    "GPUOutOfMemoryError",
    "GPUPipelineError",
    "GPUPipelineLayout",
    "GPUQuerySet",
    "GPUQueue",
    "GPURenderBundle",
    "GPURenderBundleEncoder",
    "GPURenderPassEncoder",
    "GPURenderPipeline",
    "GPUSampler",
    "GPUShaderModule",
    "GPUSupportedFeatures",
    "GPUSupportedLimits",
    "GPUTexture",
    "GPUTextureView",
    "GPUValidationError",
];

pub const CONSTANTS: &[&str] = &[
    "GPUBufferUsage",
    "GPUColorWrite",
    "GPUMapMode",
    "GPUShaderStage",
    "GPUTextureUsage",
];

pub fn reset() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
    GPU_VALUE.with(|slot| *slot.borrow_mut() = None);
    WARNING_EMITTED.with(|warned| warned.set(false));
    #[cfg(feature = "gpu")]
    {
        BUFFERS.with(|values| values.borrow_mut().clear());
        COMMAND_ENCODERS.with(|values| values.borrow_mut().clear());
        COMMAND_BUFFERS.with(|values| values.borrow_mut().clear());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_constants_match_webgpu_bit_flags() {
        assert_eq!(
            constant_value("GPUBufferUsage")
                .get_property("COPY_DST")
                .to_number(),
            8.0
        );
        assert_eq!(
            constant_value("GPUTextureUsage")
                .get_property("RENDER_ATTACHMENT")
                .to_number(),
            16.0
        );
    }

    #[test]
    fn gpu_capability_surface_is_branded_and_conservative() {
        reset();
        let gpu = gpu_value();
        assert!(w3cos_core::class::instance_of(&gpu, &class_for("GPU")));
        assert_eq!(
            gpu.call_method("getPreferredCanvasFormat", vec![])
                .to_js_string(),
            "bgra8unorm"
        );
        assert_eq!(
            gpu.get_property("wgslLanguageFeatures")
                .get_property("size")
                .to_number(),
            0.0
        );
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn native_adapter_creates_device_and_mapped_buffer_when_available() {
        reset();
        let adapter_slot = Rc::new(RefCell::new(Value::Undefined));
        let adapter_out = adapter_slot.clone();
        gpu_value()
            .call_method("requestAdapter", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *adapter_out.borrow_mut() = arg(&args, 0);
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        let adapter = adapter_slot.borrow().clone();
        if adapter.is_null() {
            return;
        }
        assert!(w3cos_core::class::instance_of(
            &adapter,
            &class_for("GPUAdapter")
        ));

        let device_slot = Rc::new(RefCell::new(Value::Undefined));
        let device_out = device_slot.clone();
        adapter.call_method("requestDevice", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *device_out.borrow_mut() = arg(&args, 0);
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        let device = device_slot.borrow().clone();
        assert!(w3cos_core::class::instance_of(
            &device,
            &class_for("GPUDevice")
        ));
        let buffer = device.call_method(
            "createBuffer",
            vec![Value::object(HashMap::from([
                ("size".into(), Value::Number(16.0)),
                (
                    "usage".into(),
                    Value::Number(
                        (wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST).bits() as f64,
                    ),
                ),
                ("mappedAtCreation".into(), Value::Bool(true)),
            ]))],
        );
        assert_eq!(buffer.get_property("mapState").to_js_string(), "mapped");
        assert_eq!(
            w3cos_core::binary::bytes_of(&buffer.call_method("getMappedRange", vec![]))
                .map(|bytes| bytes.len()),
            Some(16)
        );
        buffer.call_method("unmap", vec![]);
        assert_eq!(buffer.get_property("mapState").to_js_string(), "unmapped");
    }
}
