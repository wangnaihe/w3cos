//! Counter plugin. A click is a real function in this cdylib; the number is a
//! runtime signal. This file does not depend on `w3cos-runtime`.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

use w3cos_abi::{W3cosPluginStyle, W3COS_RUNTIME_ABI};

static COUNTER: AtomicU32 = AtomicU32::new(0);

#[link(name = "w3cos_runtime")]
unsafe extern "C" {
    fn w3cos_plugin_signal_i64(initial: i64) -> u32;
    fn w3cos_plugin_signal_get(id: u32) -> i64;
    fn w3cos_plugin_signal_set(id: u32, value: i64);
    fn w3cos_plugin_text(text: *const c_char, style: *const W3cosPluginStyle) -> u32;
    fn w3cos_plugin_button(
        label: *const c_char,
        style: *const W3cosPluginStyle,
        on_click: Option<extern "C" fn(*mut c_void)>,
        userdata: *mut c_void,
    ) -> u32;
    fn w3cos_plugin_column(
        style: *const W3cosPluginStyle,
        children: *const u32,
        count: u32,
    ) -> u32;
    fn w3cos_runtime_run_plugin(
        build: extern "C" fn(*mut c_void) -> u32,
        userdata: *mut c_void,
    ) -> i32;
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_abi_required() -> u32 {
    W3COS_RUNTIME_ABI
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_main() -> i32 {
    let id = unsafe { w3cos_plugin_signal_i64(0) };
    COUNTER.store(id, Ordering::Relaxed);
    unsafe { w3cos_runtime_run_plugin(build, std::ptr::null_mut()) }
}

extern "C" fn on_add(_userdata: *mut c_void) {
    let id = COUNTER.load(Ordering::Relaxed);
    let value = unsafe { w3cos_plugin_signal_get(id) };
    unsafe { w3cos_plugin_signal_set(id, value + 1) };
}

extern "C" fn build(_userdata: *mut c_void) -> u32 {
    let id = COUNTER.load(Ordering::Relaxed);
    let value = unsafe { w3cos_plugin_signal_get(id) };
    let label = format!("count {value}");
    let Ok(label) = std::ffi::CString::new(label) else {
        return w3cos_abi::W3COS_PLUGIN_NODE_NONE;
    };
    let column_style = W3cosPluginStyle {
        background_rgba: 0x102A_43FF,
        color_rgba: 0xF0F4_F8FF,
        gap: 16.0,
        padding: 48.0,
        align_center: 1,
        justify_center: 1,
        width: 520.0,
        height: 280.0,
        ..W3cosPluginStyle::default()
    };
    let text_style = W3cosPluginStyle {
        color_rgba: 0xF0F4_F8FF,
        font_size: 42.0,
        ..W3cosPluginStyle::default()
    };
    let button_style = W3cosPluginStyle {
        background_rgba: 0x2BB0_EDFF,
        color_rgba: 0x0414_22FF,
        font_size: 20.0,
        padding: 12.0,
        radius: 8.0,
        ..W3cosPluginStyle::default()
    };
    let text = unsafe { w3cos_plugin_text(label.as_ptr(), &text_style) };
    let button = unsafe {
        w3cos_plugin_button(c"Add".as_ptr(), &button_style, Some(on_add), std::ptr::null_mut())
    };
    let children = [text, button];
    unsafe { w3cos_plugin_column(&column_style, children.as_ptr(), children.len() as u32) }
}
