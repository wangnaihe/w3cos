//! GPU filter pass — Vello offscreen raster + wgpu separable blur + color ops.
//!
//! Filtered layers are registered via [`vello::Renderer::register_texture`] (zero CPU readback).

use std::sync::Arc;

use vello::peniko::{Blob, Color, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
use vello::wgpu;
use vello::{AaConfig, RenderParams, Renderer, Scene};

use crate::filter::{FilterChain, FilterOp};

/// GPU-resident filtered layer ready for [`Scene::draw_image`].
#[derive(Clone)]
pub struct FilteredLayerGpu {
    pub image: ImageData,
    pub width: u32,
    pub height: u32,
}

const BLUR_WGSL: &str = r#"
struct Params { dir: vec2<f32>, radius: f32, _pad: f32 }
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_smp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(pos[vi], 0.0, 1.0);
    out.uv = uv[vi];
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(src_tex));
    let texel = 1.0 / dims;
    let r = max(i32(params.radius), 1);
    var acc = vec4<f32>(0.0);
    for (var i = -r; i <= r; i++) {
        let off = params.dir * f32(i) * texel;
        acc += textureSample(src_tex, src_smp, in.uv + off);
    }
    return acc / f32(2 * r + 1);
}
"#;

const COLOR_WGSL: &str = r#"
struct Params {
    op: u32,
    v0: f32,
    v1: f32,
    v2: f32,
    v3: f32,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
}
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_smp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(pos[vi], 0.0, 1.0);
    out.uv = uv[vi];
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var c = textureSample(src_tex, src_smp, in.uv);
    let rgb = c.rgb * 255.0;
    var r = rgb.r;
    var g = rgb.g;
    var b = rgb.b;
    if (params.op == 1u) {
        r *= params.v0; g *= params.v0; b *= params.v0;
    } else if (params.op == 2u) {
        r = (r - 128.0) * params.v0 + 128.0;
        g = (g - 128.0) * params.v0 + 128.0;
        b = (b - 128.0) * params.v0 + 128.0;
    } else if (params.op == 3u) {
        let gray = 0.299 * r + 0.587 * g + 0.114 * b;
        r = r + (gray - r) * params.v0;
        g = g + (gray - g) * params.v0;
        b = b + (gray - b) * params.v0;
    } else if (params.op == 4u) {
        let sr = clamp(r * 0.393 + g * 0.769 + b * 0.189, 0.0, 255.0);
        let sg = clamp(r * 0.349 + g * 0.686 + b * 0.168, 0.0, 255.0);
        let sb = clamp(r * 0.272 + g * 0.534 + b * 0.131, 0.0, 255.0);
        r = r + (sr - r) * params.v0;
        g = g + (sg - g) * params.v0;
        b = b + (sb - b) * params.v0;
    } else if (params.op == 5u) {
        r = r + (255.0 - 2.0 * r) * params.v0;
        g = g + (255.0 - 2.0 * g) * params.v0;
        b = b + (255.0 - 2.0 * b) * params.v0;
    } else if (params.op == 6u) {
        let nr = r * params.m0.x + g * params.m0.y + b * params.m0.z;
        let ng = r * params.m1.x + g * params.m1.y + b * params.m1.z;
        let nb = r * params.m2.x + g * params.m2.y + b * params.m2.z;
        r = clamp(nr, 0.0, 255.0);
        g = clamp(ng, 0.0, 255.0);
        b = clamp(nb, 0.0, 255.0);
    } else if (params.op == 7u) {
        c.a *= params.v0;
        return c;
    }
    return vec4<f32>(clamp(r, 0.0, 255.0), clamp(g, 0.0, 255.0), clamp(b, 0.0, 255.0), c.a) / 255.0;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniform {
    dir: [f32; 2],
    radius: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorUniform {
    op: u32,
    v0: f32,
    v1: f32,
    v2: f32,
    v3: f32,
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
}

pub struct GpuFilterPipelines {
    blur_layout: wgpu::BindGroupLayout,
    blur_pipeline: wgpu::RenderPipeline,
    color_layout: wgpu::BindGroupLayout,
    color_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    blur_uniform: wgpu::Buffer,
    color_uniform: wgpu::Buffer,
}

impl GpuFilterPipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("w3cos filter sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("w3cos blur"),
            source: wgpu::ShaderSource::Wgsl(BLUR_WGSL.into()),
        });
        let color_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("w3cos color filter"),
            source: wgpu::ShaderSource::Wgsl(COLOR_WGSL.into()),
        });

        let blur_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blur uniform"),
            size: std::mem::size_of::<BlurUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let color_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("color uniform"),
            size: std::mem::size_of::<ColorUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let color_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("color bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let blur_pipeline = Self::make_pipeline(
            device,
            &blur_shader,
            &blur_layout,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let color_pipeline = Self::make_pipeline(
            device,
            &color_shader,
            &color_layout,
            wgpu::TextureFormat::Rgba8Unorm,
        );

        Self {
            blur_layout,
            blur_pipeline,
            color_layout,
            color_pipeline,
            sampler,
            blur_uniform,
            color_uniform,
        }
    }

    fn make_pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("filter pl"),
            bind_group_layouts: &[layout],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("filter pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }
}

pub struct GpuLayerTextures {
    pub width: u32,
    pub height: u32,
    vello_tex: wgpu::Texture,
    vello_view: wgpu::TextureView,
    ping_tex: wgpu::Texture,
    ping_view: wgpu::TextureView,
    pong_tex: wgpu::Texture,
    pong_view: wgpu::TextureView,
}

impl GpuLayerTextures {
    pub fn ensure(device: &wgpu::Device, pool: &mut Option<Self>, width: u32, height: u32) {
        if pool
            .as_ref()
            .is_some_and(|t| t.width >= width && t.height >= height)
        {
            return;
        }
        *pool = Some(Self::create(device, width.max(1), height.max(1)));
    }

    fn create(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let vello_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filter vello"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let ping_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filter ping"),
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
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let pong_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filter pong"),
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
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        Self {
            width,
            height,
            vello_view: vello_tex.create_view(&wgpu::TextureViewDescriptor::default()),
            ping_view: ping_tex.create_view(&wgpu::TextureViewDescriptor::default()),
            pong_view: pong_tex.create_view(&wgpu::TextureViewDescriptor::default()),
            vello_tex,
            ping_tex,
            pong_tex,
        }
    }

    pub fn vello_view(&self) -> &wgpu::TextureView {
        &self.vello_view
    }
}

fn create_output_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("filter output"),
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
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Reusable output textures — returned to the pool via [`Renderer::override_image`] on frame end.
pub struct GpuOutputTexturePool {
    free: Vec<PooledTexture>,
    active: Vec<ActiveOutput>,
}

struct PooledTexture {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
}

struct ActiveOutput {
    image: ImageData,
}

impl GpuOutputTexturePool {
    pub fn new() -> Self {
        Self {
            free: Vec::new(),
            active: Vec::new(),
        }
    }

    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    fn acquire(&mut self, device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        if let Some(idx) = best_fit_index(&self.free, width, height) {
            return self.free.swap_remove(idx).texture;
        }
        create_output_texture(device, width.max(1), height.max(1))
    }

    /// Release Vello overrides and return textures to the free pool.
    pub fn end_frame(&mut self, renderer: &mut Renderer) {
        let slots: Vec<ActiveOutput> = self.active.drain(..).collect();
        for slot in slots {
            if let Some(info) = renderer.override_image(&slot.image, None) {
                self.free.push(PooledTexture {
                    width: info.texture.width(),
                    height: info.texture.height(),
                    texture: info.texture,
                });
            }
        }
    }
}

fn best_fit_index(free: &[PooledTexture], width: u32, height: u32) -> Option<usize> {
    free.iter()
        .enumerate()
        .filter(|(_, t)| t.width >= width && t.height >= height)
        .min_by_key(|(_, t)| t.width.saturating_mul(t.height))
        .map(|(i, _)| i)
}

fn make_image_data(width: u32, height: u32) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new(Vec::<u8>::new())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }
}

pub struct GpuFilterCtx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub renderer: &'a mut Renderer,
    pub antialiasing_method: AaConfig,
    pub pipelines: &'a GpuFilterPipelines,
    pub layer_pool: &'a mut Option<GpuLayerTextures>,
    pub output_pool: &'a mut GpuOutputTexturePool,
    pub scale_factor: f32,
}

impl GpuFilterCtx<'_> {
    /// Rasterize `layer_scene`, apply `chain` on GPU, register texture with Vello (no CPU readback).
    pub fn rasterize_filtered_layer(
        &mut self,
        layer_scene: &Scene,
        width: u32,
        height: u32,
        chain: &FilterChain,
    ) -> Option<FilteredLayerGpu> {
        if width == 0 || height == 0 {
            return None;
        }
        GpuLayerTextures::ensure(self.device, self.layer_pool, width, height);
        let layer = self.layer_pool.as_ref()?;

        self.renderer
            .render_to_texture(
                self.device,
                self.queue,
                layer_scene,
                layer.vello_view(),
                &RenderParams {
                    base_color: Color::new([0.0, 0.0, 0.0, 0.0]),
                    width,
                    height,
                    antialiasing_method: self.antialiasing_method,
                },
            )
            .ok()?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("filter chain"),
            });

        // slot: 0=vello, 1=ping, 2=pong
        let mut slot: u8 = 0;
        for op in &chain.ops {
            match op {
                FilterOp::Blur(radius) if *radius > 0.0 => {
                    let r = (*radius / 2.0).max(1.0);
                    let src = Self::view_for_slot(slot, layer);
                    Self::blur_pass(
                        self.device,
                        self.queue,
                        &self.pipelines,
                        &mut encoder,
                        src,
                        &layer.ping_view,
                        width,
                        height,
                        [1.0, 0.0],
                        r,
                    );
                    Self::blur_pass(
                        self.device,
                        self.queue,
                        &self.pipelines,
                        &mut encoder,
                        &layer.ping_view,
                        &layer.pong_view,
                        width,
                        height,
                        [0.0, 1.0],
                        r,
                    );
                    slot = 2;
                }
                FilterOp::DropShadow(_) => {}
                color_op => {
                    let src = Self::view_for_slot(slot, layer);
                    let dst = if slot == 2 {
                        &layer.ping_view
                    } else {
                        &layer.pong_view
                    };
                    Self::color_pass(
                        self.device,
                        self.queue,
                        &self.pipelines,
                        &mut encoder,
                        src,
                        dst,
                        width,
                        height,
                        color_op,
                    );
                    slot = if slot == 2 { 1 } else { 2 };
                }
            }
        }

        let (final_tex, _) = match slot {
            0 => (&layer.vello_tex, false),
            1 => (&layer.ping_tex, false),
            _ => (&layer.pong_tex, true),
        };

        let output_tex = self.output_pool.acquire(self.device, width, height);
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: final_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &output_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(Some(encoder.finish()));

        let image = make_image_data(output_tex.width(), output_tex.height());
        self.renderer.override_image(
            &image,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: output_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            }),
        );
        self.output_pool.active.push(ActiveOutput {
            image: image.clone(),
        });
        Some(FilteredLayerGpu {
            image,
            width,
            height,
        })
    }

    fn view_for_slot<'b>(slot: u8, layer: &'b GpuLayerTextures) -> &'b wgpu::TextureView {
        match slot {
            1 => &layer.ping_view,
            2 => &layer.pong_view,
            _ => &layer.vello_view,
        }
    }

    fn blur_pass(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &GpuFilterPipelines,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        width: u32,
        height: u32,
        dir: [f32; 2],
        radius: f32,
    ) {
        let uniform = BlurUniform {
            dir,
            radius,
            _pad: 0.0,
        };
        queue.write_buffer(&pipelines.blur_uniform, 0, bytemuck::bytes_of(&uniform));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur bg"),
            layout: &pipelines.blur_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: pipelines.blur_uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipelines.blur_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn color_pass(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &GpuFilterPipelines,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        width: u32,
        height: u32,
        op: &FilterOp,
    ) {
        let uniform = color_uniform_for(op);
        queue.write_buffer(&pipelines.color_uniform, 0, bytemuck::bytes_of(&uniform));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("color bg"),
            layout: &pipelines.color_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: pipelines.color_uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("color filter"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipelines.color_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        let _ = (width, height);
    }
}

fn color_uniform_for(op: &FilterOp) -> ColorUniform {
    let zero4 = [0.0; 4];
    match op {
        FilterOp::Brightness(v) => ColorUniform {
            op: 1,
            v0: *v,
            v1: 0.0,
            v2: 0.0,
            v3: 0.0,
            m0: zero4,
            m1: zero4,
            m2: zero4,
        },
        FilterOp::Contrast(v) => ColorUniform {
            op: 2,
            v0: *v,
            v1: 0.0,
            v2: 0.0,
            v3: 0.0,
            m0: zero4,
            m1: zero4,
            m2: zero4,
        },
        FilterOp::Grayscale(v) => ColorUniform {
            op: 3,
            v0: *v,
            v1: 0.0,
            v2: 0.0,
            v3: 0.0,
            m0: zero4,
            m1: zero4,
            m2: zero4,
        },
        FilterOp::Sepia(v) => ColorUniform {
            op: 4,
            v0: *v,
            v1: 0.0,
            v2: 0.0,
            v3: 0.0,
            m0: zero4,
            m1: zero4,
            m2: zero4,
        },
        FilterOp::Invert(v) => ColorUniform {
            op: 5,
            v0: *v,
            v1: 0.0,
            v2: 0.0,
            v3: 0.0,
            m0: zero4,
            m1: zero4,
            m2: zero4,
        },
        FilterOp::Saturate(s) => {
            let m = crate::filter::saturate_matrix_public(*s);
            ColorUniform {
                op: 6,
                v0: 0.0,
                v1: 0.0,
                v2: 0.0,
                v3: 0.0,
                m0: [m[0][0], m[0][1], m[0][2], 0.0],
                m1: [m[1][0], m[1][1], m[1][2], 0.0],
                m2: [m[2][0], m[2][1], m[2][2], 0.0],
            }
        }
        FilterOp::HueRotate(deg) => {
            let m = crate::filter::hue_rotate_matrix_public(*deg);
            ColorUniform {
                op: 6,
                v0: 0.0,
                v1: 0.0,
                v2: 0.0,
                v3: 0.0,
                m0: [m[0][0], m[0][1], m[0][2], 0.0],
                m1: [m[1][0], m[1][1], m[1][2], 0.0],
                m2: [m[2][0], m[2][1], m[2][2], 0.0],
            }
        }
        FilterOp::Opacity(o) => ColorUniform {
            op: 7,
            v0: *o,
            v1: 0.0,
            v2: 0.0,
            v3: 0.0,
            m0: zero4,
            m1: zero4,
            m2: zero4,
        },
        FilterOp::Blur(_) | FilterOp::DropShadow(_) => ColorUniform {
            op: 0,
            v0: 1.0,
            v1: 0.0,
            v2: 0.0,
            v3: 0.0,
            m0: zero4,
            m1: zero4,
            m2: zero4,
        },
    }
}

pub fn draw_filtered_image(
    scene: &mut Scene,
    x: f32,
    y: f32,
    layer: &FilteredLayerGpu,
    dpi: vello::kurbo::Affine,
) {
    let brush = ImageBrush::new(layer.image.clone());
    let transform = dpi * vello::kurbo::Affine::translate((x as f64, y as f64));
    scene.draw_image(brush.as_ref(), transform);
}

#[cfg(test)]
mod tests {
    #[test]
    fn best_fit_picks_smallest_sufficient() {
        let sizes = [(64u32, 64u32), (256, 256), (128, 128)];
        let pick = sizes
            .iter()
            .enumerate()
            .filter(|(_, (w, h))| *w >= 100 && *h >= 80)
            .min_by_key(|(_, (w, h))| w.saturating_mul(*h))
            .map(|(i, _)| i);
        assert_eq!(pick, Some(2));
        let none = sizes
            .iter()
            .enumerate()
            .filter(|(_, (w, h))| *w >= 300 && *h >= 300)
            .min_by_key(|(_, (w, h))| w.saturating_mul(*h))
            .map(|(i, _)| i);
        assert!(none.is_none());
    }
}
