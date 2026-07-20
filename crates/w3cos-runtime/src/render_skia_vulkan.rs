//! Android Skia Ganesh presenter backed directly by an `ANativeWindow` Vulkan swapchain.
//!
//! Acquisition, Skia submission and presentation all use one graphics queue.
//! Empty queue submissions bridge the external swapchain semaphores around
//! Skia's internal command buffers without a CPU readback or per-frame idle.

// Vulkan object creation and presentation are contained in this presenter;
// each unsafe method documents that boundary rather than repeating an unsafe
// block around every ash call inside it.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::CString;
use std::ptr;
use std::time::Duration;

use ash::khr::{android_surface, surface, swapchain};
use ash::vk::{self, Handle as _};
use skia_safe::gpu::{
    self, FlushInfo, SurfaceOrigin, backend_render_targets, direct_contexts, surfaces,
    vk as skia_vk,
};
use skia_safe::{ColorType, FontMgr, Typeface};
use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

use super::render_skia::{ReplayFrame, replay_frame};

const FRAMES_IN_FLIGHT: usize = 2;
const MOBILE_SKIA_RESOURCE_CACHE_BYTES: usize = 32 * 1024 * 1024;

struct FrameSync {
    acquired: vk::Semaphore,
    rendered: vk::Semaphore,
    fence: vk::Fence,
}

pub struct SkiaVulkanPresenter {
    _entry: ash::Entry,
    instance: ash::Instance,
    surface_loader: surface::Instance,
    surface: vk::SurfaceKHR,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    swapchain_loader: swapchain::Device,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    image_initialized: Vec<bool>,
    format: vk::Format,
    extent: vk::Extent2D,
    sync: Vec<FrameSync>,
    frame: usize,
    context: Option<gpu::DirectContext>,
    typeface: Typeface,
    swapchain_dirty: bool,
}

impl SkiaVulkanPresenter {
    pub fn new(window: &winit::window::Window, font_bytes: &[u8]) -> Option<Self> {
        unsafe { Self::try_new(window, font_bytes).ok() }
    }

    unsafe fn try_new(window: &winit::window::Window, font_bytes: &[u8]) -> Result<Self, String> {
        let typeface = FontMgr::default()
            .new_from_data(font_bytes, None)
            .ok_or_else(|| "embedded font is not a valid Skia typeface".to_string())?;
        let entry = ash::Entry::load().map_err(|error| format!("load Vulkan: {error}"))?;
        let window_handle = window
            .window_handle()
            .map_err(|error| format!("ANativeWindow handle: {error}"))?;
        let RawWindowHandle::AndroidNdk(window_handle) = window_handle.as_raw() else {
            return Err("winit did not expose an Android ANativeWindow".to_string());
        };
        let extensions = [surface::NAME.as_ptr(), android_surface::NAME.as_ptr()];
        let app_name = CString::new("w3cos").expect("static application name");
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(1)
            .engine_name(&app_name)
            .engine_version(1)
            .api_version(vk::API_VERSION_1_0);
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&extensions);
        let instance = entry
            .create_instance(&instance_info, None)
            .map_err(|error| format!("create Vulkan instance: {error}"))?;
        let android_surface_loader = android_surface::Instance::new(&entry, &instance);
        let android_surface_info = vk::AndroidSurfaceCreateInfoKHR::default()
            .window(window_handle.a_native_window.as_ptr().cast());
        let surface =
            match android_surface_loader.create_android_surface(&android_surface_info, None) {
                Ok(surface) => surface,
                Err(error) => {
                    instance.destroy_instance(None);
                    return Err(format!("create Android Vulkan surface: {error}"));
                }
            };
        let surface_loader = surface::Instance::new(&entry, &instance);

        let physical_devices = match instance.enumerate_physical_devices() {
            Ok(devices) => devices,
            Err(error) => {
                surface_loader.destroy_surface(surface, None);
                instance.destroy_instance(None);
                return Err(format!("enumerate Vulkan devices: {error}"));
            }
        };
        let Some((physical_device, queue_family)) =
            physical_devices.into_iter().find_map(|physical| {
                instance
                    .get_physical_device_queue_family_properties(physical)
                    .iter()
                    .enumerate()
                    .find_map(|(index, properties)| {
                        let graphics = properties.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                        let present = surface_loader
                            .get_physical_device_surface_support(physical, index as u32, surface)
                            .unwrap_or(false);
                        (graphics && present).then_some((physical, index as u32))
                    })
            })
        else {
            surface_loader.destroy_surface(surface, None);
            instance.destroy_instance(None);
            return Err("no Vulkan graphics+present queue for ANativeWindow".to_string());
        };

        let priorities = [1.0_f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let device_extensions = [swapchain::NAME.as_ptr()];
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_extensions);
        let device = match instance.create_device(physical_device, &device_info, None) {
            Ok(device) => device,
            Err(error) => {
                surface_loader.destroy_surface(surface, None);
                instance.destroy_instance(None);
                return Err(format!("create Vulkan device: {error}"));
            }
        };
        let queue = device.get_device_queue(queue_family, 0);
        let swapchain_loader = swapchain::Device::new(&instance, &device);

        let context = {
            let get_proc = |request: skia_vk::GetProcOf| -> *const std::ffi::c_void {
                match request {
                    skia_vk::GetProcOf::Instance(raw, name) => entry
                        .get_instance_proc_addr(vk::Instance::from_raw(raw as _), name)
                        .map(|function| function as *const std::ffi::c_void)
                        .unwrap_or(ptr::null()),
                    skia_vk::GetProcOf::Device(raw, name) => instance
                        .get_device_proc_addr(vk::Device::from_raw(raw as _), name)
                        .map(|function| function as *const std::ffi::c_void)
                        .unwrap_or(ptr::null()),
                }
            };
            let backend = skia_vk::BackendContext::new_with_extensions(
                instance.handle().as_raw() as _,
                physical_device.as_raw() as _,
                device.handle().as_raw() as _,
                (queue.as_raw() as _, queue_family as usize),
                &get_proc,
                &["VK_KHR_surface", "VK_KHR_android_surface"],
                &["VK_KHR_swapchain"],
            );
            direct_contexts::make_vulkan(&backend, None)
        };
        let mut context = match context {
            Some(context) => context,
            None => {
                device.destroy_device(None);
                surface_loader.destroy_surface(surface, None);
                instance.destroy_instance(None);
                return Err("Skia could not create a Vulkan Ganesh context".to_string());
            }
        };
        let default_cache_limit = context.resource_cache_limit();
        context.set_resource_cache_limit(MOBILE_SKIA_RESOURCE_CACHE_BYTES);
        eprintln!(
            "[W3C OS] Skia Vulkan cache limit={}MiB (default={}MiB)",
            MOBILE_SKIA_RESOURCE_CACHE_BYTES / (1024 * 1024),
            default_cache_limit / (1024 * 1024),
        );

        let mut presenter = Self {
            _entry: entry,
            instance,
            surface_loader,
            surface,
            physical_device,
            device,
            queue,
            queue_family,
            swapchain_loader,
            swapchain: vk::SwapchainKHR::null(),
            images: Vec::new(),
            image_initialized: Vec::new(),
            format: vk::Format::UNDEFINED,
            extent: vk::Extent2D::default(),
            sync: Vec::new(),
            frame: 0,
            context: Some(context),
            typeface,
            swapchain_dirty: true,
        };
        presenter.create_frame_sync()?;
        let size = window.inner_size();
        presenter.recreate_swapchain(size.width, size.height)?;
        Ok(presenter)
    }

    unsafe fn create_frame_sync(&mut self) -> Result<(), String> {
        for _ in 0..FRAMES_IN_FLIGHT {
            let acquired = self
                .device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                .map_err(|error| format!("create acquire semaphore: {error}"))?;
            let rendered = match self
                .device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            {
                Ok(semaphore) => semaphore,
                Err(error) => {
                    self.device.destroy_semaphore(acquired, None);
                    return Err(format!("create render semaphore: {error}"));
                }
            };
            let fence = match self.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            ) {
                Ok(fence) => fence,
                Err(error) => {
                    self.device.destroy_semaphore(rendered, None);
                    self.device.destroy_semaphore(acquired, None);
                    return Err(format!("create frame fence: {error}"));
                }
            };
            self.sync.push(FrameSync {
                acquired,
                rendered,
                fence,
            });
        }
        Ok(())
    }

    unsafe fn recreate_swapchain(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("zero-sized Android surface".to_string());
        }
        self.device
            .device_wait_idle()
            .map_err(|error| format!("wait before swapchain recreation: {error}"))?;
        let capabilities = self
            .surface_loader
            .get_physical_device_surface_capabilities(self.physical_device, self.surface)
            .map_err(|error| format!("query surface capabilities: {error}"))?;
        let formats = self
            .surface_loader
            .get_physical_device_surface_formats(self.physical_device, self.surface)
            .map_err(|error| format!("query surface formats: {error}"))?;
        let chosen = formats
            .iter()
            .copied()
            .find(|format| format.format == vk::Format::B8G8R8A8_UNORM)
            .or_else(|| {
                formats
                    .iter()
                    .copied()
                    .find(|format| format.format == vk::Format::R8G8B8A8_UNORM)
            })
            .or_else(|| formats.first().copied())
            .ok_or_else(|| "Android Vulkan surface exposes no formats".to_string())?;
        let extent = if capabilities.current_extent.width != u32::MAX {
            capabilities.current_extent
        } else {
            vk::Extent2D {
                width: width.clamp(
                    capabilities.min_image_extent.width,
                    capabilities.max_image_extent.width,
                ),
                height: height.clamp(
                    capabilities.min_image_extent.height,
                    capabilities.max_image_extent.height,
                ),
            }
        };
        let mut image_count = capabilities.min_image_count.saturating_add(1).max(2);
        if capabilities.max_image_count > 0 {
            image_count = image_count.min(capabilities.max_image_count);
        }
        let alpha_bits = capabilities.supported_composite_alpha.as_raw();
        let composite_alpha = if capabilities
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
        {
            vk::CompositeAlphaFlagsKHR::OPAQUE
        } else {
            vk::CompositeAlphaFlagsKHR::from_raw(alpha_bits & alpha_bits.wrapping_neg())
        };
        let old_swapchain = self.swapchain;
        let create_info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(image_count)
            .image_format(chosen.format)
            .image_color_space(chosen.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(capabilities.current_transform)
            .composite_alpha(composite_alpha)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old_swapchain);
        let swapchain = self
            .swapchain_loader
            .create_swapchain(&create_info, None)
            .map_err(|error| format!("create Android swapchain: {error}"))?;
        let images = match self.swapchain_loader.get_swapchain_images(swapchain) {
            Ok(images) => images,
            Err(error) => {
                self.swapchain_loader.destroy_swapchain(swapchain, None);
                return Err(format!("get swapchain images: {error}"));
            }
        };
        if old_swapchain != vk::SwapchainKHR::null() {
            self.swapchain_loader.destroy_swapchain(old_swapchain, None);
        }
        self.swapchain = swapchain;
        self.images = images;
        self.image_initialized = vec![false; self.images.len()];
        self.format = chosen.format;
        self.extent = extent;
        self.swapchain_dirty = false;
        Ok(())
    }

    pub fn invalidate_swapchain(&mut self) {
        self.swapchain_dirty = true;
    }

    /// Release all recreatable Ganesh resources after an OS memory warning.
    ///
    /// Swapchain images are owned by Android/Vulkan and remain valid; Skia
    /// pipelines, atlases and scratch textures are rebuilt on the next frame.
    pub fn purge_cached_resources(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
        }
        if let Some(context) = self.context.as_mut() {
            context
                .perform_deferred_cleanup(Duration::ZERO, gpu::PurgeResourceOptions::AllResources);
            context.free_gpu_resources();
        }
        skia_safe::graphics::purge_all_caches();
    }

    pub fn render_frame(&mut self, width: u32, height: u32, frame: ReplayFrame<'_>) -> bool {
        let result = unsafe { self.render_frame_inner(width, height, frame) };
        match result {
            Ok(rendered) => rendered,
            Err(error) => {
                eprintln!("[W3C OS] Skia Vulkan frame failed: {error}");
                unsafe { self.recover_frame_sync() };
                false
            }
        }
    }

    /// A failed frame may happen after its fence was reset but before a
    /// submission signalled it. Recreate the small sync ring so the next frame
    /// can never block forever on that stale unsignalled fence.
    unsafe fn recover_frame_sync(&mut self) {
        let _ = self.device.device_wait_idle();
        for sync in self.sync.drain(..) {
            self.device.destroy_fence(sync.fence, None);
            self.device.destroy_semaphore(sync.rendered, None);
            self.device.destroy_semaphore(sync.acquired, None);
        }
        self.frame = 0;
        self.swapchain_dirty = true;
        if let Err(error) = self.create_frame_sync() {
            eprintln!("[W3C OS] recreate Vulkan frame sync failed: {error}");
        }
    }

    unsafe fn render_frame_inner(
        &mut self,
        width: u32,
        height: u32,
        frame: ReplayFrame<'_>,
    ) -> Result<bool, String> {
        if self.swapchain_dirty || self.extent.width != width || self.extent.height != height {
            self.recreate_swapchain(width, height)?;
        }
        let sync = &self.sync[self.frame];
        self.device
            .wait_for_fences(&[sync.fence], true, u64::MAX)
            .map_err(|error| format!("wait for frame fence: {error}"))?;
        let (image_index, suboptimal) = match self.swapchain_loader.acquire_next_image(
            self.swapchain,
            u64::MAX,
            sync.acquired,
            vk::Fence::null(),
        ) {
            Ok(result) => result,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.swapchain_dirty = true;
                return Ok(false);
            }
            Err(error) => return Err(format!("acquire swapchain image: {error}")),
        };
        self.device
            .reset_fences(&[sync.fence])
            .map_err(|error| format!("reset frame fence: {error}"))?;

        let wait_semaphores = [sync.acquired];
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let acquire_submit = [vk::SubmitInfo::default()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)];
        self.device
            .queue_submit(self.queue, &acquire_submit, vk::Fence::null())
            .map_err(|error| format!("queue swapchain acquire wait: {error}"))?;

        let (skia_format, color_type) = match self.format {
            vk::Format::B8G8R8A8_UNORM => (skia_vk::Format::B8G8R8A8_UNORM, ColorType::BGRA8888),
            vk::Format::R8G8B8A8_UNORM => (skia_vk::Format::R8G8B8A8_UNORM, ColorType::RGBA8888),
            format => return Err(format!("unsupported Android swapchain format {format:?}")),
        };
        let image_index = image_index as usize;
        let initial_layout = if self.image_initialized[image_index] {
            skia_vk::ImageLayout::PRESENT_SRC_KHR
        } else {
            skia_vk::ImageLayout::UNDEFINED
        };
        let image_info = skia_vk::ImageInfo::new(
            self.images[image_index].as_raw() as _,
            skia_vk::Alloc::default(),
            skia_vk::ImageTiling::OPTIMAL,
            initial_layout,
            skia_format,
            1,
            self.queue_family,
            None,
            None,
            None,
        );
        let target = backend_render_targets::make_vk(
            (self.extent.width as i32, self.extent.height as i32),
            &image_info,
        );
        let context = self
            .context
            .as_mut()
            .ok_or_else(|| "Skia Vulkan context was abandoned".to_string())?;
        let mut surface = surfaces::wrap_backend_render_target(
            context,
            &target,
            SurfaceOrigin::TopLeft,
            color_type,
            None,
            None,
        )
        .ok_or_else(|| "wrap Android swapchain image for Skia".to_string())?;
        replay_frame(surface.canvas(), &self.typeface, frame);
        let present_state = skia_vk::mutable_texture_states::new_vulkan(
            skia_vk::ImageLayout::PRESENT_SRC_KHR,
            self.queue_family,
        );
        context.flush_surface_with_texture_state(
            &mut surface,
            &FlushInfo::default(),
            Some(&present_state),
        );
        if !context.submit(None) {
            return Err("submit Skia Vulkan command buffers".to_string());
        }
        drop(surface);

        let signal_semaphores = [sync.rendered];
        let render_submit = [vk::SubmitInfo::default().signal_semaphores(&signal_semaphores)];
        self.device
            .queue_submit(self.queue, &render_submit, sync.fence)
            .map_err(|error| format!("signal rendered swapchain image: {error}"))?;
        let present_wait = [sync.rendered];
        let swapchains = [self.swapchain];
        let image_indices = [image_index as u32];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&present_wait)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        let present_suboptimal = match self
            .swapchain_loader
            .queue_present(self.queue, &present_info)
        {
            Ok(value) => value,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => true,
            Err(error) => return Err(format!("present Android swapchain: {error}")),
        };
        self.swapchain_dirty = suboptimal || present_suboptimal;
        self.image_initialized[image_index] = true;
        self.frame = (self.frame + 1) % self.sync.len();
        Ok(true)
    }
}

impl Drop for SkiaVulkanPresenter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(mut context) = self.context.take() {
                context.abandon();
                drop(context);
            }
            for sync in self.sync.drain(..) {
                self.device.destroy_fence(sync.fence, None);
                self.device.destroy_semaphore(sync.rendered, None);
                self.device.destroy_semaphore(sync.acquired, None);
            }
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader
                    .destroy_swapchain(self.swapchain, None);
            }
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}
