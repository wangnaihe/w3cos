//! Framework-neutral boundary between the native window loop and a DOM host.
//!
//! The native runtime delivers browser-shaped input through this interface and
//! observes pending host commits without depending on React, Vue, or another
//! framework. A host adapter is optional; the standard DOM path may dispatch
//! directly through `Document` and an application without an adapter remains
//! a valid static component tree.

use std::cell::RefCell;
use std::rc::Rc;

#[allow(clippy::too_many_arguments)]
pub trait HostRuntime {
    fn has_pending_render(&self) -> bool {
        false
    }

    fn clear_pending_render(&self) {}

    fn take_scroll_requests(&self) -> Vec<(u64, Option<f32>, Option<f32>)> {
        Vec::new()
    }

    fn dispatch_scroll(&self, _host_id: u64, _offset: f32) {}

    fn dispatch_click_chain(&self, _host_ids: &[u64]) -> bool {
        false
    }

    fn dispatch_focus_chain(&self, _host_ids: &[u64], _focused: bool) -> bool {
        false
    }

    fn dispatch_key_chain(
        &self,
        _host_ids: &[u64],
        _key: &str,
        _code: &str,
        _repeat: bool,
        _alt_key: bool,
        _ctrl_key: bool,
        _meta_key: bool,
        _shift_key: bool,
        _key_down: bool,
    ) -> bool {
        false
    }

    fn dispatch_submit_chain(&self, _host_ids: &[u64]) -> Option<bool> {
        None
    }

    fn host_local_name(&self, _host_id: u64) -> Option<String> {
        None
    }

    fn dispatch_pointer_chain(
        &self,
        _host_ids: &[u64],
        _phase: &str,
        _client_x: f32,
        _client_y: f32,
        _pointer_id: i64,
        _pointer_type: &str,
        _button: i16,
        _buttons: u16,
        _pressure: f32,
        _primary: bool,
        _alt_key: bool,
        _ctrl_key: bool,
        _meta_key: bool,
        _shift_key: bool,
    ) -> bool {
        false
    }

    fn dispatch_wheel_chain(
        &self,
        _host_ids: &[u64],
        _client_x: f32,
        _client_y: f32,
        _delta_x: f32,
        _delta_y: f32,
        _delta_mode: u8,
        _alt_key: bool,
        _ctrl_key: bool,
        _meta_key: bool,
        _shift_key: bool,
    ) -> bool {
        false
    }

    fn dispatch_before_input_chain(
        &self,
        _host_ids: &[u64],
        _data: &str,
        _input_type: &str,
        _is_composing: bool,
    ) -> bool {
        false
    }

    fn dispatch_input_chain(
        &self,
        _host_ids: &[u64],
        _value: String,
        _data: &str,
        _input_type: &str,
        _is_composing: bool,
    ) {
    }

    fn dispatch_composition_chain(&self, _host_ids: &[u64], _phase: &str, _data: &str) {}
}

thread_local! {
    static HOST_RUNTIME: RefCell<Option<Rc<dyn HostRuntime>>> = RefCell::new(None);
}

pub fn install(runtime: Rc<dyn HostRuntime>) {
    HOST_RUNTIME.with(|slot| *slot.borrow_mut() = Some(runtime));
}

pub fn clear() {
    HOST_RUNTIME.with(|slot| *slot.borrow_mut() = None);
}

fn current() -> Option<Rc<dyn HostRuntime>> {
    HOST_RUNTIME.with(|slot| slot.borrow().clone())
}

pub fn has_pending_render() -> bool {
    current().is_some_and(|runtime| runtime.has_pending_render())
}

pub fn clear_pending_render() {
    if let Some(runtime) = current() {
        runtime.clear_pending_render();
    }
}

pub fn take_scroll_requests() -> Vec<(u64, Option<f32>, Option<f32>)> {
    current()
        .map(|runtime| runtime.take_scroll_requests())
        .unwrap_or_default()
}

pub fn dispatch_scroll(host_id: u64, offset: f32) {
    if let Some(runtime) = current() {
        runtime.dispatch_scroll(host_id, offset);
    }
}

pub fn dispatch_click(host_id: u64) -> bool {
    dispatch_click_chain(&[host_id])
}

pub fn dispatch_click_chain(host_ids: &[u64]) -> bool {
    current().is_some_and(|runtime| runtime.dispatch_click_chain(host_ids))
}

pub fn dispatch_focus_chain(host_ids: &[u64], focused: bool) -> bool {
    current().is_some_and(|runtime| runtime.dispatch_focus_chain(host_ids, focused))
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_key_chain(
    host_ids: &[u64],
    key: &str,
    code: &str,
    repeat: bool,
    alt_key: bool,
    ctrl_key: bool,
    meta_key: bool,
    shift_key: bool,
    key_down: bool,
) -> bool {
    current().is_some_and(|runtime| {
        runtime.dispatch_key_chain(
            host_ids, key, code, repeat, alt_key, ctrl_key, meta_key, shift_key, key_down,
        )
    })
}

pub fn dispatch_submit_chain(host_ids: &[u64]) -> Option<bool> {
    current().and_then(|runtime| runtime.dispatch_submit_chain(host_ids))
}

pub fn host_local_name(host_id: u64) -> Option<String> {
    current().and_then(|runtime| runtime.host_local_name(host_id))
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_pointer_chain(
    host_ids: &[u64],
    phase: &str,
    client_x: f32,
    client_y: f32,
    pointer_id: i64,
    pointer_type: &str,
    button: i16,
    buttons: u16,
    pressure: f32,
    primary: bool,
    alt_key: bool,
    ctrl_key: bool,
    meta_key: bool,
    shift_key: bool,
) -> bool {
    current().is_some_and(|runtime| {
        runtime.dispatch_pointer_chain(
            host_ids,
            phase,
            client_x,
            client_y,
            pointer_id,
            pointer_type,
            button,
            buttons,
            pressure,
            primary,
            alt_key,
            ctrl_key,
            meta_key,
            shift_key,
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_wheel_chain(
    host_ids: &[u64],
    client_x: f32,
    client_y: f32,
    delta_x: f32,
    delta_y: f32,
    delta_mode: u8,
    alt_key: bool,
    ctrl_key: bool,
    meta_key: bool,
    shift_key: bool,
) -> bool {
    current().is_some_and(|runtime| {
        runtime.dispatch_wheel_chain(
            host_ids, client_x, client_y, delta_x, delta_y, delta_mode, alt_key, ctrl_key,
            meta_key, shift_key,
        )
    })
}

pub fn dispatch_before_input_chain(
    host_ids: &[u64],
    data: &str,
    input_type: &str,
    is_composing: bool,
) -> bool {
    current().is_some_and(|runtime| {
        runtime.dispatch_before_input_chain(host_ids, data, input_type, is_composing)
    })
}

pub fn dispatch_input_chain(
    host_ids: &[u64],
    value: String,
    data: &str,
    input_type: &str,
    is_composing: bool,
) {
    if let Some(runtime) = current() {
        runtime.dispatch_input_chain(host_ids, value, data, input_type, is_composing);
    }
}

pub fn dispatch_composition_chain(host_ids: &[u64], phase: &str, data: &str) {
    if let Some(runtime) = current() {
        runtime.dispatch_composition_chain(host_ids, phase, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct ClickHost(Rc<Cell<u64>>);

    impl HostRuntime for ClickHost {
        fn dispatch_click_chain(&self, host_ids: &[u64]) -> bool {
            self.0.set(host_ids.first().copied().unwrap_or_default());
            true
        }
    }

    #[test]
    fn installed_host_receives_framework_neutral_dispatch() {
        let clicked = Rc::new(Cell::new(0));
        install(Rc::new(ClickHost(Rc::clone(&clicked))));
        assert!(dispatch_click(42));
        assert_eq!(clicked.get(), 42);
        clear();
        assert!(!dispatch_click(42));
    }
}
