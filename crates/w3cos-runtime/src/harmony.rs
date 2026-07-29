//! HarmonyOS NEXT renderer host.
//!
//! ArkUI owns lifecycle and supplies an `OHNativeWindow` through XComponent.
//! This module deliberately bypasses winit: it creates an EGL/GLES3 context,
//! wraps the default framebuffer with Skia Ganesh, and replays the same W3COS
//! component tree used by the other native platforms.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::ptr;

use skia_safe::gpu::{SurfaceOrigin, backend_render_targets, direct_contexts, gl, surfaces};
use skia_safe::{ColorType, FontMgr, Typeface};
use w3cos_std::component::{Component, EventAction};

use crate::layout;
use crate::render_skia::{ReplayFrame, replay_frame};

type EglBoolean = c_uint;
type EglDisplay = *mut c_void;
type EglConfig = *mut c_void;
type EglContext = *mut c_void;
type EglSurface = *mut c_void;

const EGL_FALSE: EglBoolean = 0;
const EGL_DEFAULT_DISPLAY: *mut c_void = ptr::null_mut();
const EGL_NO_DISPLAY: EglDisplay = ptr::null_mut();
const EGL_NO_CONTEXT: EglContext = ptr::null_mut();
const EGL_NO_SURFACE: EglSurface = ptr::null_mut();
const EGL_NONE: c_int = 0x3038;
const EGL_RED_SIZE: c_int = 0x3024;
const EGL_GREEN_SIZE: c_int = 0x3023;
const EGL_BLUE_SIZE: c_int = 0x3022;
const EGL_ALPHA_SIZE: c_int = 0x3021;
const EGL_STENCIL_SIZE: c_int = 0x3026;
const EGL_SURFACE_TYPE: c_int = 0x3033;
const EGL_WINDOW_BIT: c_int = 0x0004;
const EGL_RENDERABLE_TYPE: c_int = 0x3040;
const EGL_OPENGL_ES3_BIT: c_int = 0x0040;
const EGL_CONTEXT_CLIENT_VERSION: c_int = 0x3098;
const EGL_OPENGL_ES_API: c_uint = 0x30A0;

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetDisplay(display_id: *mut c_void) -> EglDisplay;
    fn eglInitialize(display: EglDisplay, major: *mut c_int, minor: *mut c_int) -> EglBoolean;
    fn eglBindAPI(api: c_uint) -> EglBoolean;
    fn eglChooseConfig(
        display: EglDisplay,
        attributes: *const c_int,
        configs: *mut EglConfig,
        config_size: c_int,
        count: *mut c_int,
    ) -> EglBoolean;
    fn eglCreateContext(
        display: EglDisplay,
        config: EglConfig,
        share: EglContext,
        attributes: *const c_int,
    ) -> EglContext;
    fn eglCreateWindowSurface(
        display: EglDisplay,
        config: EglConfig,
        window: *mut c_void,
        attributes: *const c_int,
    ) -> EglSurface;
    fn eglMakeCurrent(
        display: EglDisplay,
        draw: EglSurface,
        read: EglSurface,
        context: EglContext,
    ) -> EglBoolean;
    fn eglSwapInterval(display: EglDisplay, interval: c_int) -> EglBoolean;
    fn eglSwapBuffers(display: EglDisplay, surface: EglSurface) -> EglBoolean;
    fn eglDestroySurface(display: EglDisplay, surface: EglSurface) -> EglBoolean;
    fn eglDestroyContext(display: EglDisplay, context: EglContext) -> EglBoolean;
    fn eglTerminate(display: EglDisplay) -> EglBoolean;
    fn eglGetProcAddress(name: *const c_char) -> *const c_void;
    fn eglGetError() -> c_int;
}

enum AppSource {
    Component(fn() -> Component),
    Dom(fn()),
}

struct HarmonyRuntime {
    display: EglDisplay,
    context: EglContext,
    surface: EglSurface,
    width: u32,
    height: u32,
    source: AppSource,
    root: Component,
    direct_context: skia_safe::gpu::DirectContext,
    typeface: Typeface,
    pressed_target: Option<u32>,
}

thread_local! {
    static RUNTIME: RefCell<Option<HarmonyRuntime>> = const { RefCell::new(None) };
}

impl HarmonyRuntime {
    fn new(
        native_window: *mut c_void,
        width: u32,
        height: u32,
        source: AppSource,
    ) -> Result<Self, String> {
        if native_window.is_null() || width == 0 || height == 0 {
            return Err("invalid OHNativeWindow or surface size".into());
        }

        let root = match source {
            AppSource::Component(builder) => builder(),
            AppSource::Dom(setup) => {
                crate::dom::reset_document();
                setup();
                crate::jsdom::drain_bootstrap_tasks(64);
                crate::dom::clear_document_dirty();
                crate::dom::to_component_tree()
            }
        };

        let (display, context, surface) = create_egl(native_window)?;
        let interface = gl::Interface::new_native()
            .or_else(|| {
                gl::Interface::new_load_with(|name| {
                    let Ok(name) = CString::new(name) else {
                        return ptr::null();
                    };
                    unsafe { eglGetProcAddress(name.as_ptr()) }
                })
            })
            .ok_or_else(|| "Skia could not create an OHOS GL interface".to_string())?;
        let direct_context = direct_contexts::make_gl(interface, None)
            .ok_or_else(|| "Skia could not create an OHOS Ganesh context".to_string())?;
        let font = crate::font_face::host_ui_font();
        let typeface = FontMgr::default()
            .new_from_data(font.data.as_slice(), Some(font.index as usize))
            .ok_or_else(|| "Host system font is invalid".to_string())?;

        let mut runtime = Self {
            display,
            context,
            surface,
            width,
            height,
            source,
            root,
            direct_context,
            typeface,
            pressed_target: None,
        };
        runtime.render()?;
        Ok(runtime)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.width = width;
        self.height = height;
        self.render()
    }

    fn frame(&mut self) -> Result<(), String> {
        let mut work = crate::jsdom::tick_timers();
        work += crate::jsdom::drain_microtasks();
        if matches!(self.source, AppSource::Dom(_)) && crate::dom::is_document_dirty() {
            crate::dom::clear_document_dirty();
            self.root = crate::dom::to_component_tree();
            work += 1;
        }
        if work > 0 || crate::state::is_dirty() {
            if let AppSource::Component(builder) = self.source {
                self.root = builder();
                crate::state::clear_dirty();
            }
            self.render()?;
        }
        Ok(())
    }

    fn render(&mut self) -> Result<(), String> {
        if unsafe { eglMakeCurrent(self.display, self.surface, self.surface, self.context) }
            == EGL_FALSE
        {
            return Err(egl_error("eglMakeCurrent"));
        }

        let layout = layout::compute(&self.root, self.width as f32, self.height as f32)
            .map_err(|error| format!("OHOS layout failed: {error:#}"))?;
        let flat = layout::pre_flatten(&self.root);
        let nodes: Vec<_> = layout
            .iter()
            .filter_map(|(rect, index)| {
                let node = flat.get(*index)?;
                Some((*index, *rect, node.kind, node.style))
            })
            .collect();
        let scroll_info = vec![None; flat.len()];
        let text_inputs = HashMap::new();
        let target = backend_render_targets::make_gl(
            (self.width as i32, self.height as i32),
            0,
            8,
            gl::FramebufferInfo {
                fboid: 0,
                format: gl::Format::RGBA8.into(),
                ..Default::default()
            },
        );
        let mut surface = surfaces::wrap_backend_render_target(
            &mut self.direct_context,
            &target,
            SurfaceOrigin::BottomLeft,
            ColorType::RGBA8888,
            None,
            None,
        )
        .ok_or_else(|| "Skia could not wrap the OHOS EGL framebuffer".to_string())?;
        replay_frame(
            surface.canvas(),
            &self.typeface,
            ReplayFrame {
                nodes: &nodes,
                metrics_font: layout::layout_font(),
                scroll_info: &scroll_info,
                text_input_values: &text_inputs,
                focused_index: None,
                background: self.root.style.background,
                artifact: None,
            },
        );
        self.direct_context.flush_and_submit();
        drop(surface);
        if unsafe { eglSwapBuffers(self.display, self.surface) } == EGL_FALSE {
            return Err(egl_error("eglSwapBuffers"));
        }
        Ok(())
    }

    fn touch(&mut self, phase: &str, x: f32, y: f32, pointer_id: i64, pressure: f32) {
        let target = self.hit_test(x, y);
        if phase == "down" {
            self.pressed_target = target;
        }
        if let Some(target) = target {
            crate::jsdom::dispatch_native_pointer(
                target, phase, x, y, pointer_id, "touch", 0, 1, pressure, true, false, false,
                false, false,
            );
            if phase == "up" && self.pressed_target == Some(target) {
                crate::jsdom::dispatch_native_click(target);
            }
        }
        if matches!(phase, "up" | "cancel") {
            self.pressed_target = None;
        }
        let _ = self.frame();
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<u32> {
        let layout = layout::compute(&self.root, self.width as f32, self.height as f32).ok()?;
        let flat = layout::pre_flatten(&self.root);
        layout.iter().rev().find_map(|(rect, index)| {
            if x < rect.x || x > rect.x + rect.width || y < rect.y || y > rect.y + rect.height {
                return None;
            }
            let node = flat.get(*index)?;
            match node.on_click {
                EventAction::NativeHost { id, .. } => u32::try_from(*id).ok(),
                _ => None,
            }
        })
    }
}

impl Drop for HarmonyRuntime {
    fn drop(&mut self) {
        self.direct_context.abandon();
        unsafe {
            let _ = eglMakeCurrent(self.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
            let _ = eglDestroySurface(self.display, self.surface);
            let _ = eglDestroyContext(self.display, self.context);
            let _ = eglTerminate(self.display);
        }
    }
}

fn create_egl(native_window: *mut c_void) -> Result<(EglDisplay, EglContext, EglSurface), String> {
    unsafe {
        let display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
        if display == EGL_NO_DISPLAY {
            return Err(egl_error("eglGetDisplay"));
        }
        let mut major = 0;
        let mut minor = 0;
        if eglInitialize(display, &mut major, &mut minor) == EGL_FALSE {
            return Err(egl_error("eglInitialize"));
        }
        if eglBindAPI(EGL_OPENGL_ES_API) == EGL_FALSE {
            return Err(egl_error("eglBindAPI"));
        }
        let attributes = [
            EGL_SURFACE_TYPE,
            EGL_WINDOW_BIT,
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_ES3_BIT,
            EGL_RED_SIZE,
            8,
            EGL_GREEN_SIZE,
            8,
            EGL_BLUE_SIZE,
            8,
            EGL_ALPHA_SIZE,
            8,
            EGL_STENCIL_SIZE,
            8,
            EGL_NONE,
        ];
        let mut config = ptr::null_mut();
        let mut count = 0;
        if eglChooseConfig(display, attributes.as_ptr(), &mut config, 1, &mut count) == EGL_FALSE
            || count == 0
        {
            let _ = eglTerminate(display);
            return Err(egl_error("eglChooseConfig"));
        }
        let context_attributes = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
        let context =
            eglCreateContext(display, config, EGL_NO_CONTEXT, context_attributes.as_ptr());
        if context == EGL_NO_CONTEXT {
            let _ = eglTerminate(display);
            return Err(egl_error("eglCreateContext"));
        }
        let surface = eglCreateWindowSurface(display, config, native_window, ptr::null());
        if surface == EGL_NO_SURFACE {
            let _ = eglDestroyContext(display, context);
            let _ = eglTerminate(display);
            return Err(egl_error("eglCreateWindowSurface"));
        }
        if eglMakeCurrent(display, surface, surface, context) == EGL_FALSE {
            let _ = eglDestroySurface(display, surface);
            let _ = eglDestroyContext(display, context);
            let _ = eglTerminate(display);
            return Err(egl_error("eglMakeCurrent"));
        }
        let _ = eglSwapInterval(display, 1);
        Ok((display, context, surface))
    }
}

fn egl_error(operation: &str) -> String {
    format!("{operation} failed with EGL error 0x{:x}", unsafe {
        eglGetError()
    })
}

pub fn surface_created_dom(
    native_window: *mut c_void,
    width: u32,
    height: u32,
    setup: fn(),
) -> Result<(), String> {
    RUNTIME.with(|runtime| {
        *runtime.borrow_mut() = Some(HarmonyRuntime::new(
            native_window,
            width,
            height,
            AppSource::Dom(setup),
        )?);
        Ok(())
    })
}

pub fn surface_created_component(
    native_window: *mut c_void,
    width: u32,
    height: u32,
    builder: fn() -> Component,
) -> Result<(), String> {
    RUNTIME.with(|runtime| {
        *runtime.borrow_mut() = Some(HarmonyRuntime::new(
            native_window,
            width,
            height,
            AppSource::Component(builder),
        )?);
        Ok(())
    })
}

pub fn surface_changed(width: u32, height: u32) -> Result<(), String> {
    RUNTIME.with(|runtime| {
        runtime
            .borrow_mut()
            .as_mut()
            .ok_or_else(|| "OHOS surface is not initialized".to_string())?
            .resize(width, height)
    })
}

pub fn surface_destroyed() {
    RUNTIME.with(|runtime| {
        runtime.borrow_mut().take();
    });
}

pub fn frame() -> Result<(), String> {
    RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        match runtime.as_mut() {
            Some(runtime) => runtime.frame(),
            None => Ok(()),
        }
    })
}

pub fn touch(phase: &str, x: f32, y: f32, pointer_id: i64, pressure: f32) {
    RUNTIME.with(|runtime| {
        if let Some(runtime) = runtime.borrow_mut().as_mut() {
            runtime.touch(phase, x, y, pointer_id, pressure);
        }
    });
}
