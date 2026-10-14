//! Versioned C calls for an application plugin that has signals and callbacks.
//!
//! The plugin is its own cdylib. It does not link this crate as an rlib. It
//! resolves these symbols from the one `libw3cos_runtime.so`. A click is an
//! `extern "C"` function pointer; the runtime invokes it and then rebuilds the
//! tree by calling the plugin's build function again.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};

use w3cos_abi::{W3cosPluginStyle, W3COS_PLUGIN_NODE_NONE};
use w3cos_std::style::{AlignItems, Dimension, Edges, JustifyContent, Style};
use w3cos_std::{Component, EventAction};

struct Callback {
    function: extern "C" fn(*mut c_void),
    userdata: *mut c_void,
}

thread_local! {
    static NODES: RefCell<Vec<Component>> = const { RefCell::new(Vec::new()) };
    static CALLBACKS: RefCell<Vec<Callback>> = const { RefCell::new(Vec::new()) };
    static BUILD: RefCell<Option<(extern "C" fn(*mut c_void) -> u32, *mut c_void)>> =
        const { RefCell::new(None) };
}

fn plugin_invoke(code: u8, _draft: &str) {
    let callback = CALLBACKS.with(|slots| {
        slots
            .borrow()
            .get(code as usize)
            .map(|callback| (callback.function, callback.userdata))
    });
    if let Some((function, userdata)) = callback {
        function(userdata);
    }
}

fn plugin_rebuild() -> Component {
    NODES.with(|nodes| nodes.borrow_mut().clear());
    CALLBACKS.with(|slots| slots.borrow_mut().clear());
    let built = BUILD.with(|build| {
        build.borrow().map(|(function, userdata)| unsafe { function(userdata) })
    });
    let Some(root) = built else {
        return Component::text("plugin build missing", Style::default());
    };
    NODES.with(|nodes| {
        nodes
            .borrow()
            .get(root as usize)
            .cloned()
            .unwrap_or_else(|| Component::text("plugin build returned no node", Style::default()))
    })
}

fn push_node(component: Component) -> u32 {
    NODES.with(|nodes| {
        let mut nodes = nodes.borrow_mut();
        if nodes.len() >= u32::MAX as usize {
            return W3COS_PLUGIN_NODE_NONE;
        }
        let id = nodes.len() as u32;
        nodes.push(component);
        id
    })
}

fn read_c_str(text: *const c_char) -> Option<String> {
    if text.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned())
}

fn style_from_plugin(style: *const W3cosPluginStyle) -> Style {
    let Some(style) = (unsafe { style.as_ref() }) else {
        return Style::default();
    };
    let mut out = Style::default();
    if style.background_rgba & 0xff != 0 {
        out.background = color_from_rgba(style.background_rgba);
    }
    if style.color_rgba & 0xff != 0 {
        out.color = color_from_rgba(style.color_rgba);
    }
    if style.font_size > 0.0 {
        out.font_size = style.font_size;
    }
    if style.gap > 0.0 {
        out.gap = style.gap;
    }
    if style.padding > 0.0 {
        out.padding = Edges::all(style.padding);
    }
    if style.radius > 0.0 {
        out.border_radius = style.radius;
    }
    if style.width > 0.0 {
        out.width = Dimension::Px(style.width);
    }
    if style.height > 0.0 {
        out.height = Dimension::Px(style.height);
    }
    if style.grow > 0.0 {
        out.flex_grow = style.grow;
    }
    if style.align_center != 0 {
        out.align_items = AlignItems::Center;
    }
    if style.justify_center != 0 {
        out.justify_content = JustifyContent::Center;
    }
    out
}

fn color_from_rgba(packed: u32) -> w3cos_std::Color {
    w3cos_std::Color::rgba(
        (packed >> 24) as u8,
        (packed >> 16) as u8,
        (packed >> 8) as u8,
        packed as u8,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_signal_i64(initial: i64) -> u32 {
    crate::state::create_signal(initial) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_signal_get(id: u32) -> i64 {
    crate::state::get_signal(id as usize)
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_signal_set(id: u32, value: i64) {
    crate::state::set_signal(id as usize, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_text(text: *const c_char, style: *const W3cosPluginStyle) -> u32 {
    let Some(text) = read_c_str(text) else {
        crate::shared_abi::set_error("plugin text pointer is null");
        return W3COS_PLUGIN_NODE_NONE;
    };
    push_node(Component::text(text, style_from_plugin(style)))
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_button(
    label: *const c_char,
    style: *const W3cosPluginStyle,
    on_click: Option<extern "C" fn(*mut c_void)>,
    userdata: *mut c_void,
) -> u32 {
    let Some(label) = read_c_str(label) else {
        crate::shared_abi::set_error("plugin button label is null");
        return W3COS_PLUGIN_NODE_NONE;
    };
    let action = if let Some(function) = on_click {
        let code = CALLBACKS.with(|slots| {
            let mut slots = slots.borrow_mut();
            if slots.len() >= 256 {
                return None;
            }
            let code = slots.len() as u8;
            slots.push(Callback { function, userdata });
            Some(code)
        });
        let Some(code) = code else {
            crate::shared_abi::set_error("plugin click table is full");
            return W3COS_PLUGIN_NODE_NONE;
        };
        EventAction::Invoke {
            code,
            input_placeholder: String::new(),
        }
    } else {
        EventAction::None
    };
    push_node(Component::button_with_click(
        label,
        style_from_plugin(style),
        action,
    ))
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_plugin_column(
    style: *const W3cosPluginStyle,
    children: *const u32,
    count: u32,
) -> u32 {
    if count > 0 && children.is_null() {
        crate::shared_abi::set_error("plugin column children pointer is null");
        return W3COS_PLUGIN_NODE_NONE;
    }
    let ids = if count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(children, count as usize) }
    };
    let collected = NODES.with(|nodes| {
        let nodes = nodes.borrow();
        let mut collected = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(child) = nodes.get(*id as usize) else {
                return None;
            };
            collected.push(child.clone());
        }
        Some(collected)
    });
    let Some(collected) = collected else {
        crate::shared_abi::set_error("plugin column child is not a node");
        return W3COS_PLUGIN_NODE_NONE;
    };
    push_node(Component::column(style_from_plugin(style), collected))
}

/// Present `build`'s tree. Later signal writes call `build` again.
/// Create signals before this; creating them inside `build` would add a slot
/// on every rebuild.
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_runtime_run_plugin(
    build: Option<extern "C" fn(*mut c_void) -> u32>,
    userdata: *mut c_void,
) -> i32 {
    let Some(build) = build else {
        crate::shared_abi::set_error("plugin build function is null");
        return 2;
    };
    BUILD.with(|slot| *slot.borrow_mut() = Some((build, userdata)));
    crate::state::set_invoke_hook(plugin_invoke);
    match crate::run_app(plugin_rebuild) {
        Ok(()) => 0,
        Err(error) => {
            crate::shared_abi::set_error(error.to_string());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static BUMPED: AtomicU32 = AtomicU32::new(0);

    extern "C" fn bump(data: *mut c_void) {
        let id = data as usize as u32;
        let value = unsafe { w3cos_plugin_signal_get(id) };
        unsafe { w3cos_plugin_signal_set(id, value + 1) };
        BUMPED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn click_callback_increments_the_signal_without_linking_a_second_runtime() {
        BUMPED.store(0, Ordering::SeqCst);
        let id = w3cos_plugin_signal_i64(0);
        assert_eq!(w3cos_plugin_signal_get(id), 0);
        NODES.with(|nodes| nodes.borrow_mut().clear());
        CALLBACKS.with(|slots| slots.borrow_mut().clear());
        let style = W3cosPluginStyle {
            background_rgba: 0x2BB0_EDFF,
            color_rgba: 0x0414_22FF,
            font_size: 18.0,
            radius: 8.0,
            ..W3cosPluginStyle::default()
        };
        let label = c"Add".as_ptr();
        let button = unsafe { w3cos_plugin_button(label, &style, Some(bump), id as usize as *mut c_void) };
        assert_ne!(button, W3COS_PLUGIN_NODE_NONE);
        crate::state::set_invoke_hook(plugin_invoke);
        let action = NODES.with(|nodes| nodes.borrow()[button as usize].on_click.clone());
        crate::state::execute_action(&action);
        assert_eq!(w3cos_plugin_signal_get(id), 1);
        assert_eq!(BUMPED.load(Ordering::SeqCst), 1);
        crate::state::clear_dirty();
        crate::state::clear_invoke_hook();
    }
}
