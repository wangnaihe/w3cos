//! C-ABI-facing HarmonyOS host operations.

use std::ffi::c_void;

use w3cos_std::Component;

pub fn surface_created_dom(window: *mut c_void, width: u32, height: u32, setup: fn()) -> i32 {
    result_code(w3cos_runtime::harmony::surface_created_dom(
        window, width, height, setup,
    ))
}

pub fn surface_created_component(
    window: *mut c_void,
    width: u32,
    height: u32,
    builder: fn() -> Component,
) -> i32 {
    result_code(w3cos_runtime::harmony::surface_created_component(
        window, width, height, builder,
    ))
}

pub fn surface_changed(width: u32, height: u32) -> i32 {
    result_code(w3cos_runtime::harmony::surface_changed(width, height))
}

pub fn surface_destroyed() {
    w3cos_runtime::harmony::surface_destroyed();
}

pub fn frame() -> i32 {
    result_code(w3cos_runtime::harmony::frame())
}

pub fn touch(phase: i32, x: f32, y: f32, pointer_id: i64, pressure: f32) {
    let phase = match phase {
        0 => "down",
        1 => "up",
        2 => "move",
        3 => "cancel",
        _ => return,
    };
    w3cos_runtime::harmony::touch(phase, x, y, pointer_id, pressure);
}

fn result_code(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            log::error!("W3COS HarmonyOS host failed: {error}");
            1
        }
    }
}
