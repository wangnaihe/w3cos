//! GPU swapchain present without a per-frame CPU readback.
//!
//! Vello renders to an intermediate texture; the vsync path blits that to the
//! wgpu surface and calls [`present_swapchain`]. Copying the framebuffer to
//! CPU (`copy_texture_to_buffer` / `map_async` / `to_vec`) is a separate
//! function used only when a screenshot or snapshot was requested.

use std::sync::atomic::{AtomicU64, Ordering};

use vello::wgpu;

static GPU_PRESENTS: AtomicU64 = AtomicU64::new(0);
static GPU_CPU_READBACKS: AtomicU64 = AtomicU64::new(0);

/// Frames submitted with [`present_swapchain`].
pub fn gpu_present_count() -> u64 {
    GPU_PRESENTS.load(Ordering::Relaxed)
}

/// Full-framebuffer GPU→CPU copies. Present must not increment this.
pub fn gpu_cpu_readback_count() -> u64 {
    GPU_CPU_READBACKS.load(Ordering::Relaxed)
}

/// Which GPU copies the vsync path is allowed to issue.
///
/// `copy_framebuffer_to_cpu` is the buffer-map / `to_vec` path. It is off
/// unless a screenshot or snapshot was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPresentPlan {
    pub blit_to_swapchain: bool,
    pub submit_and_present: bool,
    pub copy_framebuffer_to_cpu: bool,
}

pub fn gpu_present_plan(screenshot_requested: bool) -> GpuPresentPlan {
    GpuPresentPlan {
        blit_to_swapchain: true,
        submit_and_present: true,
        copy_framebuffer_to_cpu: screenshot_requested,
    }
}

pub(crate) fn record_gpu_present() {
    GPU_PRESENTS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_gpu_cpu_readback() {
    GPU_CPU_READBACKS.fetch_add(1, Ordering::Relaxed);
}

/// Submit the surface blit and present the swapchain texture.
///
/// This is the vsync hot path. Do not map a buffer or copy the framebuffer
/// to CPU here.
pub fn present_swapchain(
    queue: &wgpu::Queue,
    encoder: wgpu::CommandEncoder,
    surface_texture: wgpu::SurfaceTexture,
) {
    record_gpu_present();
    queue.submit([encoder.finish()]);
    surface_texture.present();
}

fn padded_bytes_per_row(width: u32) -> u32 {
    let unpadded = width.saturating_mul(4);
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(align) * align
}

/// GPU→CPU readback for screenshots. Never called from [`present_swapchain`].
///
/// `source` is Vello's intermediate target (`TEXTURE_BINDING`, typically
/// without `COPY_SRC`). We blit into a `COPY_SRC` texture, then map. Headless
/// CI has no swapchain; unit tests cover the split, not this GPU copy.
pub fn copy_texture_view_to_cpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::TextureView,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    record_gpu_cpu_readback();
    if width == 0 || height == 0 {
        return None;
    }

    let readback_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("w3cos-screenshot-readback"),
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
    let readback_view = readback_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let blitter = wgpu::util::TextureBlitter::new(device, wgpu::TextureFormat::Rgba8Unorm);

    let bytes_per_row = padded_bytes_per_row(width);
    let buffer_size = u64::from(bytes_per_row) * u64::from(height);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("w3cos-screenshot-map"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("w3cos-screenshot-readback"),
    });
    blitter.copy(device, &mut encoder, source, &readback_view);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &readback_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    rx.recv().ok()?.ok()?;

    let mapped = slice.get_mapped_range();
    let tight = (width as usize) * 4;
    let padded = bytes_per_row as usize;
    let mut rgba = vec![0u8; tight * height as usize];
    for y in 0..height as usize {
        let src = &mapped[y * padded..y * padded + tight];
        rgba[y * tight..(y + 1) * tight].copy_from_slice(src);
    }
    drop(mapped);
    buffer.unmap();
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_present_skips_cpu_readback_unless_screenshot_requested() {
        let vsync = gpu_present_plan(false);
        assert!(vsync.blit_to_swapchain);
        assert!(vsync.submit_and_present);
        assert!(
            !vsync.copy_framebuffer_to_cpu,
            "vsync present must not map or to_vec the framebuffer"
        );

        let snapshot = gpu_present_plan(true);
        assert!(snapshot.submit_and_present);
        assert!(
            snapshot.copy_framebuffer_to_cpu,
            "screenshots may still read back on demand"
        );
    }

    #[test]
    fn present_swapchain_counter_does_not_count_as_readback() {
        let presents = gpu_present_count();
        let readbacks = gpu_cpu_readback_count();
        record_gpu_present();
        assert_eq!(gpu_present_count(), presents + 1);
        assert_eq!(
            gpu_cpu_readback_count(),
            readbacks,
            "present must not increment the CPU readback counter"
        );
        record_gpu_cpu_readback();
        assert_eq!(gpu_cpu_readback_count(), readbacks + 1);
    }
}
