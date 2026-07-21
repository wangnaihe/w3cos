//! React hooks compatibility layer for W3C OS.
//!
//! Mirrors the React Hooks API on top of the [`w3cos_core`] signal system,
//! so a function-component style render API can be authored verbatim:
//!
//! ```ignore
//! use w3cos_react_compat::*;
//!
//! fn Counter() {
//!     let count = use_state::<i64>(|| 0);
//!     let label = use_memo(|| format!("count={}", count.get()), &[count.deps()]);
//!
//!     use_effect(
//!         || println!("rendered: {}", label.get()),
//!         &[count.deps()],
//!     );
//!
//!     let inc = use_callback(
//!         {
//!             let count = count.clone();
//!             move || count.set(count.get() + 1)
//!         },
//!         &[count.deps()],
//!     );
//!     inc();
//! }
//! ```
//!
//! ## Hook semantics
//!
//! React tracks hooks by their *call order* within a component body. We do
//! the same: each render frame the runtime calls
//! [`begin_render(component_id)`](begin_render) and the hook calls allocate
//! slot `0`, `1`, `2`, … from the per-component slot table. The slot table
//! survives across renders — that's how `use_state` keeps its value.
//!
//! When a `useState` setter is invoked, the corresponding `Signal` notifies
//! its subscribers, which in turn schedule the component for re-render via
//! [`mark_dirty`]. The host loop drives that re-render by calling
//! [`begin_render`] again with the same `component_id` and replaying the
//! component body.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::{Effect, Signal, batch};

/// Dynamic host ABI used by the npm ESM AOT pipeline. These functions expose
/// React's public runtime contract in terms of `w3cos_core::Value`; third-party
/// component code is still compiled from its real package source.
pub mod aot {
    #![allow(non_snake_case)]

    use super::{use_effect, use_memo, use_ref, use_state};
    use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
    use std::hash::{Hash, Hasher};
    use std::rc::Rc;
    use w3cos_core::Value;
    use w3cos_std::color::Color;
    use w3cos_std::component::Component;
    use w3cos_std::style::{
        AlignContent, AlignItems, AlignSelf, BoxShadow, Cursor, Dimension, Display, Easing, Edges,
        FlexDirection, FlexWrap, FontStyle, JustifyContent, OutlineStyle, Overflow, PointerEvents,
        Position, Spacing, Style, TextAlign, TextDecoration, TextOverflow, Transform2D, Transition,
        TransitionProperty, UserSelect, Visibility, WhiteSpace, WillChange, WordBreak,
    };

    thread_local! {
        static NEXT_AOT_COMPONENT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
        static NEXT_HOST_ORDINALS: std::cell::RefCell<HashMap<u64, u64>> = std::cell::RefCell::new(HashMap::new());
        static AOT_COMPONENT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static CURRENT_AOT_COMPONENT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
        static DIRTY_AOT_COMPONENTS: std::cell::RefCell<HashSet<u64>> = std::cell::RefCell::new(HashSet::new());
        static DIRTY_AOT_ANCESTORS: std::cell::RefCell<HashSet<u64>> = std::cell::RefCell::new(HashSet::new());
        static ACTIVE_AOT_COMPONENTS: std::cell::RefCell<HashSet<u64>> = std::cell::RefCell::new(HashSet::new());
        static LAST_AOT_COMPONENTS: std::cell::RefCell<HashSet<u64>> = std::cell::RefCell::new(HashSet::new());
        static COMPONENT_PARENTS: std::cell::RefCell<HashMap<u64, Option<u64>>> = std::cell::RefCell::new(HashMap::new());
        static MEMO_BAILOUT_COMPONENTS: std::cell::RefCell<HashSet<u64>> = std::cell::RefCell::new(HashSet::new());
        static MEMO_INSTANCES: std::cell::RefCell<HashMap<u64, (Value, Value)>> = std::cell::RefCell::new(HashMap::new());
        static COMPONENT_INPUT_CACHE: std::cell::RefCell<HashMap<u64, Value>> = std::cell::RefCell::new(HashMap::new());
        static COMPONENT_VALUE_CACHE: std::cell::RefCell<HashMap<u64, Value>> = std::cell::RefCell::new(HashMap::new());
        static COMPONENT_OUTPUT_CACHE: std::cell::RefCell<HashMap<u64, (Option<Style>, Vec<Component>)>> = std::cell::RefCell::new(HashMap::new());
        static HOST_STYLE_CACHE: std::cell::RefCell<HashMap<u64, (Value, Option<Style>, Style)>> = std::cell::RefCell::new(HashMap::new());
        static HOST_ELEMENTS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static PENDING_LAYOUT_EFFECTS: std::cell::RefCell<Vec<(usize, Box<dyn FnOnce()>)>> = std::cell::RefCell::new(Vec::new());
        static PENDING_EFFECTS: std::cell::RefCell<Vec<(usize, Box<dyn FnOnce()>)>> = std::cell::RefCell::new(Vec::new());
        static SCROLL_REQUESTS: std::cell::RefCell<std::collections::HashMap<u64, (Option<f32>, Option<f32>)>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static SCROLL_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static SCROLL_PROP_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static CLICK_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static INPUT_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static CHANGE_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static BEFORE_INPUT_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static COMPOSITION_START_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static COMPOSITION_UPDATE_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static COMPOSITION_END_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static FOCUS_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static BLUR_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static KEYDOWN_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static KEYUP_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static SUBMIT_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static POINTER_DOWN_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static POINTER_UP_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static POINTER_MOVE_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static POINTER_ENTER_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static POINTER_LEAVE_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static POINTER_CANCEL_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static MOUSE_DOWN_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static MOUSE_UP_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static MOUSE_MOVE_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static MOUSE_ENTER_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static MOUSE_LEAVE_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static WHEEL_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }

    fn deps(value: &Value) -> Vec<u64> {
        let Value::Array(values) = value else {
            return Vec::new();
        };
        values.borrow().iter().map(Value::identity_hash).collect()
    }

    pub fn call_host(path: &str, arguments: Vec<Value>) -> Value {
        if path == "w3cos_core::host::invoke" {
            return w3cos_core::host::invoke(arguments);
        }
        let argument = |index| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match path.rsplit("::").next().unwrap_or(path) {
            "useState" => useState(argument(0)),
            "useMemo" => useMemo(argument(0), argument(1)),
            "useCallback" => useCallback(argument(0), argument(1)),
            "useRef" => useRef(argument(0)),
            "useEffect" => useEffect(argument(0), argument(1)),
            "useLayoutEffect" => useLayoutEffect(argument(0), argument(1)),
            "useImperativeHandle" => useImperativeHandle(argument(0), argument(1), argument(2)),
            "memo" => memo(argument(0), argument(1)),
            "createElement" => createElement(arguments),
            "jsx" | "jsxs" => jsx_runtime(arguments),
            "Fragment" => Fragment(),
            _ => Value::Undefined,
        }
    }

    pub fn render_to_component(value: Value) -> Component {
        NEXT_AOT_COMPONENT.with(|next| next.set(1));
        NEXT_HOST_ORDINALS.with(|ordinals| ordinals.borrow_mut().clear());
        MEMO_BAILOUT_COMPONENTS.with(|components| components.borrow_mut().clear());
        ACTIVE_AOT_COMPONENTS.with(|components| components.borrow_mut().clear());
        let mut rendered = render_children(value, None, 0xcbf2_9ce4_8422_2325, 0, false, None);
        DIRTY_AOT_COMPONENTS.with(|components| components.borrow_mut().clear());
        let component = if rendered.len() == 1 {
            rendered.pop().unwrap()
        } else {
            Component::root(rendered)
        };
        prune_unmounted(&component);
        flush_pending_effects();
        component
    }

    fn prune_unmounted(root: &Component) {
        let active_components =
            ACTIVE_AOT_COMPONENTS.with(|components| components.borrow().clone());
        let previous_components = LAST_AOT_COMPONENTS.with(|components| {
            std::mem::replace(&mut *components.borrow_mut(), active_components.clone())
        });
        for component_id in previous_components.difference(&active_components).copied() {
            super::unmount(super::ComponentId(component_id));
        }
        COMPONENT_PARENTS.with(|parents| {
            parents
                .borrow_mut()
                .retain(|component_id, _| active_components.contains(component_id));
        });
        for cache in [&COMPONENT_INPUT_CACHE, &COMPONENT_VALUE_CACHE] {
            cache.with(|cache| {
                cache
                    .borrow_mut()
                    .retain(|component_id, _| active_components.contains(component_id));
            });
        }
        COMPONENT_OUTPUT_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .retain(|component_id, _| active_components.contains(component_id));
        });
        MEMO_INSTANCES.with(|instances| {
            instances
                .borrow_mut()
                .retain(|component_id, _| active_components.contains(component_id));
        });

        let mut active_hosts = HashSet::new();
        collect_host_ids(root, &mut active_hosts);
        HOST_ELEMENTS.with(|elements| {
            elements
                .borrow_mut()
                .retain(|host_id, _| active_hosts.contains(host_id));
        });
        HOST_STYLE_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .retain(|host_id, _| active_hosts.contains(host_id));
        });
        SCROLL_REQUESTS.with(|requests| {
            requests
                .borrow_mut()
                .retain(|host_id, _| active_hosts.contains(host_id));
        });
        for listeners in [
            &SCROLL_LISTENERS,
            &SCROLL_PROP_LISTENERS,
            &CLICK_LISTENERS,
            &INPUT_LISTENERS,
            &CHANGE_LISTENERS,
            &BEFORE_INPUT_LISTENERS,
            &COMPOSITION_START_LISTENERS,
            &COMPOSITION_UPDATE_LISTENERS,
            &COMPOSITION_END_LISTENERS,
            &FOCUS_LISTENERS,
            &BLUR_LISTENERS,
            &KEYDOWN_LISTENERS,
            &KEYUP_LISTENERS,
            &SUBMIT_LISTENERS,
            &POINTER_DOWN_LISTENERS,
            &POINTER_UP_LISTENERS,
            &POINTER_MOVE_LISTENERS,
            &POINTER_ENTER_LISTENERS,
            &POINTER_LEAVE_LISTENERS,
            &POINTER_CANCEL_LISTENERS,
            &MOUSE_DOWN_LISTENERS,
            &MOUSE_UP_LISTENERS,
            &MOUSE_MOVE_LISTENERS,
            &MOUSE_ENTER_LISTENERS,
            &MOUSE_LEAVE_LISTENERS,
            &WHEEL_LISTENERS,
        ] {
            listeners.with(|listeners| {
                listeners
                    .borrow_mut()
                    .retain(|host_id, _| active_hosts.contains(host_id));
            });
        }
    }

    fn collect_host_ids(component: &Component, host_ids: &mut HashSet<u64>) {
        if let w3cos_std::EventAction::NativeHost { id, .. } = component.on_click {
            host_ids.insert(id);
        }
        for child in &component.children {
            collect_host_ids(child, host_ids);
        }
    }

    fn flush_pending_effects() {
        let mut layout_effects =
            PENDING_LAYOUT_EFFECTS.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
        layout_effects.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        if std::env::var_os("W3COS_AOT_TRACE").is_some() {
            eprintln!(
                "[w3cos-aot] flushing {} layout effects",
                layout_effects.len()
            );
        }
        for (_, effect) in layout_effects {
            effect();
        }
        if super::has_dirty() {
            if std::env::var_os("W3COS_AOT_TRACE").is_some() {
                eprintln!("[w3cos-aot] deferring passive effects until the commit is stable");
            }
            return;
        }
        let mut effects =
            PENDING_EFFECTS.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
        effects.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        if std::env::var_os("W3COS_AOT_TRACE").is_some() {
            eprintln!("[w3cos-aot] flushing {} passive effects", effects.len());
        }
        for (_, effect) in effects {
            effect();
        }
    }

    pub fn dispatch_scroll(host_id: u64, offset: f32) {
        let element = HOST_ELEMENTS.with(|elements| {
            if let Some(element) = elements.borrow().get(&host_id) {
                element.set_property("scrollTop", Value::Number(offset as f64));
                Some(element.clone())
            } else {
                None
            }
        });
        let event = Value::object(std::collections::HashMap::new());
        if let Some(element) = element {
            event.set_property("target", element.clone());
            event.set_property("currentTarget", element);
        }
        let listener = SCROLL_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned());
        if let Some(listener) = listener {
            listener.call(Value::Undefined, vec![event.clone()]);
        }
        let prop_listener =
            SCROLL_PROP_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned());
        if let Some(listener) = prop_listener {
            listener.call(Value::Undefined, vec![event]);
        }
    }

    pub fn take_scroll_requests() -> Vec<(u64, Option<f32>, Option<f32>)> {
        SCROLL_REQUESTS.with(|requests| {
            std::mem::take(&mut *requests.borrow_mut())
                .into_iter()
                .map(|(host_id, (left, top))| (host_id, left, top))
                .collect()
        })
    }

    pub fn dispatch_click(host_id: u64) -> bool {
        dispatch_click_chain(&[host_id])
    }

    pub fn dispatch_click_chain(host_ids: &[u64]) -> bool {
        let target = host_ids.first().and_then(|host_id| {
            HOST_ELEMENTS.with(|elements| elements.borrow().get(host_id).cloned())
        });
        let propagation_stopped = Rc::new(std::cell::Cell::new(false));
        let default_prevented = Rc::new(std::cell::Cell::new(false));
        let mut dispatched = false;
        for host_id in host_ids {
            let listener =
                CLICK_LISTENERS.with(|listeners| listeners.borrow().get(host_id).cloned());
            let Some(listener) = listener else {
                continue;
            };
            dispatched = true;
            let event = Value::object(std::collections::HashMap::new());
            event.set_property("type", Value::from("click"));
            event.set_property("bubbles", Value::Bool(true));
            event.set_property("cancelable", Value::Bool(true));
            event.set_property("defaultPrevented", Value::Bool(false));
            if let Some(target) = target.clone() {
                event.set_property("target", target);
            }
            if let Some(current_target) =
                HOST_ELEMENTS.with(|elements| elements.borrow().get(host_id).cloned())
            {
                event.set_property("currentTarget", current_target);
            }
            let stopped = Rc::clone(&propagation_stopped);
            event.set_property(
                "stopPropagation",
                Value::function(move |_, _| {
                    stopped.set(true);
                    Value::Undefined
                }),
            );
            let prevented = Rc::clone(&default_prevented);
            let prevented_event = event.clone();
            event.set_property(
                "preventDefault",
                Value::function(move |_, _| {
                    if prevented_event.get_property("cancelable").to_bool() {
                        prevented.set(true);
                        prevented_event.set_property("defaultPrevented", Value::Bool(true));
                    }
                    Value::Undefined
                }),
            );
            listener.call(Value::Undefined, vec![event]);
            if propagation_stopped.get() {
                break;
            }
        }
        dispatched
    }

    pub fn dispatch_focus_chain(host_ids: &[u64], focused: bool) -> bool {
        dispatch_simple_bubbling_event(
            host_ids,
            if focused { "focus" } else { "blur" },
            |host_id| {
                if focused {
                    FOCUS_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned())
                } else {
                    BLUR_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned())
                }
            },
            |_| {},
        )
        .0
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
        dispatch_simple_bubbling_event(
            host_ids,
            if key_down { "keydown" } else { "keyup" },
            |host_id| {
                if key_down {
                    KEYDOWN_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned())
                } else {
                    KEYUP_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned())
                }
            },
            |event| {
                event.set_property("key", Value::from(key));
                event.set_property("code", Value::from(code));
                event.set_property("repeat", Value::Bool(repeat));
                event.set_property("altKey", Value::Bool(alt_key));
                event.set_property("ctrlKey", Value::Bool(ctrl_key));
                event.set_property("metaKey", Value::Bool(meta_key));
                event.set_property("shiftKey", Value::Bool(shift_key));
            },
        )
        .1
    }

    pub fn dispatch_submit_chain(host_ids: &[u64]) -> Option<bool> {
        let Some(form_index) = host_ids.iter().position(|host_id| {
            SUBMIT_LISTENERS.with(|listeners| listeners.borrow().contains_key(host_id))
        }) else {
            return None;
        };
        let target_is_textarea = host_ids.first().is_some_and(|host_id| {
            HOST_ELEMENTS.with(|elements| {
                elements
                    .borrow()
                    .get(host_id)
                    .is_some_and(|host| host.get_property("localName").to_js_string() == "textarea")
            })
        });
        if target_is_textarea {
            return None;
        }
        Some(
            dispatch_simple_bubbling_event(
                &host_ids[form_index..],
                "submit",
                |host_id| {
                    SUBMIT_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned())
                },
                |_| {},
            )
            .1,
        )
    }

    pub fn host_local_name(host_id: u64) -> Option<String> {
        HOST_ELEMENTS.with(|elements| {
            elements
                .borrow()
                .get(&host_id)
                .map(|host| host.get_property("localName").to_js_string())
        })
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
        let pointer_event_type = format!("pointer{phase}");
        let pointer_result = dispatch_simple_bubbling_event(
            host_ids,
            &pointer_event_type,
            |host_id| match phase {
                "down" => POINTER_DOWN_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                "up" => {
                    POINTER_UP_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned())
                }
                "move" => POINTER_MOVE_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                "enter" => POINTER_ENTER_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                "leave" => POINTER_LEAVE_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                "cancel" => POINTER_CANCEL_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                _ => None,
            },
            |event| {
                decorate_point_event(event, client_x, client_y, button, buttons);
                decorate_modifiers(event, alt_key, ctrl_key, meta_key, shift_key);
                event.set_property("pointerId", Value::Number(pointer_id as f64));
                event.set_property("pointerType", Value::from(pointer_type));
                event.set_property("isPrimary", Value::Bool(primary));
                event.set_property("pressure", Value::Number(pressure as f64));
                event.set_property("width", Value::Number(1.0));
                event.set_property("height", Value::Number(1.0));
                if matches!(phase, "enter" | "leave") {
                    event.set_property("bubbles", Value::Bool(false));
                }
            },
        );
        let mouse_result = if pointer_type == "mouse" && phase != "cancel" {
            let mouse_event_type = format!("mouse{phase}");
            dispatch_simple_bubbling_event(
                host_ids,
                &mouse_event_type,
                |host_id| match phase {
                    "down" => MOUSE_DOWN_LISTENERS
                        .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                    "up" => MOUSE_UP_LISTENERS
                        .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                    "move" => MOUSE_MOVE_LISTENERS
                        .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                    "enter" => MOUSE_ENTER_LISTENERS
                        .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                    "leave" => MOUSE_LEAVE_LISTENERS
                        .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                    _ => None,
                },
                |event| {
                    decorate_point_event(event, client_x, client_y, button, buttons);
                    decorate_modifiers(event, alt_key, ctrl_key, meta_key, shift_key);
                    if matches!(phase, "enter" | "leave") {
                        event.set_property("bubbles", Value::Bool(false));
                    }
                },
            )
        } else {
            (false, false)
        };
        pointer_result.1 || mouse_result.1
    }

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
        dispatch_simple_bubbling_event(
            host_ids,
            "wheel",
            |host_id| WHEEL_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned()),
            |event| {
                decorate_point_event(event, client_x, client_y, 0, 0);
                decorate_modifiers(event, alt_key, ctrl_key, meta_key, shift_key);
                event.set_property("deltaX", Value::Number(delta_x as f64));
                event.set_property("deltaY", Value::Number(delta_y as f64));
                event.set_property("deltaZ", Value::Number(0.0));
                event.set_property("deltaMode", Value::Number(delta_mode as f64));
            },
        )
        .1
    }

    fn decorate_point_event(
        event: &Value,
        client_x: f32,
        client_y: f32,
        button: i16,
        buttons: u16,
    ) {
        event.set_property("clientX", Value::Number(client_x as f64));
        event.set_property("clientY", Value::Number(client_y as f64));
        event.set_property("pageX", Value::Number(client_x as f64));
        event.set_property("pageY", Value::Number(client_y as f64));
        event.set_property("screenX", Value::Number(client_x as f64));
        event.set_property("screenY", Value::Number(client_y as f64));
        event.set_property("button", Value::Number(button as f64));
        event.set_property("buttons", Value::Number(buttons as f64));
    }

    fn decorate_modifiers(
        event: &Value,
        alt_key: bool,
        ctrl_key: bool,
        meta_key: bool,
        shift_key: bool,
    ) {
        event.set_property("altKey", Value::Bool(alt_key));
        event.set_property("ctrlKey", Value::Bool(ctrl_key));
        event.set_property("metaKey", Value::Bool(meta_key));
        event.set_property("shiftKey", Value::Bool(shift_key));
    }

    fn dispatch_simple_bubbling_event(
        host_ids: &[u64],
        event_type: &str,
        listener_for: impl Fn(u64) -> Option<Value>,
        decorate: impl Fn(&Value),
    ) -> (bool, bool) {
        let target = host_ids.first().and_then(|host_id| {
            HOST_ELEMENTS.with(|elements| elements.borrow().get(host_id).cloned())
        });
        let propagation_stopped = Rc::new(std::cell::Cell::new(false));
        let default_prevented = Rc::new(std::cell::Cell::new(false));
        let mut dispatched = false;
        for host_id in host_ids {
            let Some(listener) = listener_for(*host_id) else {
                continue;
            };
            dispatched = true;
            let event = Value::object(std::collections::HashMap::new());
            event.set_property("type", Value::from(event_type));
            event.set_property("bubbles", Value::Bool(true));
            event.set_property("cancelable", Value::Bool(true));
            event.set_property("defaultPrevented", Value::Bool(false));
            if let Some(target) = target.clone() {
                event.set_property("target", target);
            }
            if let Some(current_target) =
                HOST_ELEMENTS.with(|elements| elements.borrow().get(host_id).cloned())
            {
                event.set_property("currentTarget", current_target);
            }
            decorate(&event);
            let stopped = Rc::clone(&propagation_stopped);
            event.set_property(
                "stopPropagation",
                Value::function(move |_, _| {
                    stopped.set(true);
                    Value::Undefined
                }),
            );
            let prevented = Rc::clone(&default_prevented);
            let prevented_event = event.clone();
            event.set_property(
                "preventDefault",
                Value::function(move |_, _| {
                    prevented.set(true);
                    prevented_event.set_property("defaultPrevented", Value::Bool(true));
                    Value::Undefined
                }),
            );
            listener.call(Value::Undefined, vec![event]);
            if propagation_stopped.get() {
                break;
            }
        }
        (dispatched, default_prevented.get())
    }

    pub fn dispatch_before_input_chain(
        host_ids: &[u64],
        data: &str,
        input_type: &str,
        is_composing: bool,
    ) -> bool {
        dispatch_simple_bubbling_event(
            host_ids,
            "beforeinput",
            |host_id| {
                BEFORE_INPUT_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned())
            },
            |event| decorate_input_event(event, data, input_type, is_composing),
        )
        .1
    }

    pub fn dispatch_input_chain(
        host_ids: &[u64],
        value: String,
        data: &str,
        input_type: &str,
        is_composing: bool,
    ) {
        if let Some(host_id) = host_ids.first() {
            HOST_ELEMENTS.with(|elements| {
                if let Some(element) = elements.borrow().get(host_id) {
                    element.set_property("value", Value::from(value));
                }
            });
        }
        for (event_type, listeners) in [("input", &INPUT_LISTENERS), ("change", &CHANGE_LISTENERS)]
        {
            dispatch_simple_bubbling_event(
                host_ids,
                event_type,
                |host_id| listeners.with(|map| map.borrow().get(&host_id).cloned()),
                |event| {
                    event.set_property("cancelable", Value::Bool(false));
                    decorate_input_event(event, data, input_type, is_composing);
                },
            );
        }
    }

    pub fn dispatch_composition_chain(host_ids: &[u64], phase: &str, data: &str) {
        let event_type = format!("composition{phase}");
        dispatch_simple_bubbling_event(
            host_ids,
            &event_type,
            |host_id| match phase {
                "start" => COMPOSITION_START_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                "update" => COMPOSITION_UPDATE_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                "end" => COMPOSITION_END_LISTENERS
                    .with(|listeners| listeners.borrow().get(&host_id).cloned()),
                _ => None,
            },
            |event| {
                event.set_property("data", Value::from(data));
                event.set_property("isComposing", Value::Bool(phase != "end"));
            },
        );
    }

    fn decorate_input_event(event: &Value, data: &str, input_type: &str, is_composing: bool) {
        event.set_property("data", Value::from(data));
        event.set_property("inputType", Value::from(input_type));
        event.set_property("isComposing", Value::Bool(is_composing));
    }

    pub fn has_pending_render() -> bool {
        super::has_dirty()
    }

    pub fn clear_pending_render() {
        let dirty = super::take_dirty();
        let dirty: HashSet<u64> = dirty.into_iter().map(|id| id.0).collect();
        let ancestors =
            COMPONENT_PARENTS.with(|parents| dirty_component_ancestors(&dirty, &parents.borrow()));
        DIRTY_AOT_COMPONENTS.with(|components| {
            let mut components = components.borrow_mut();
            components.clear();
            components.extend(dirty);
        });
        DIRTY_AOT_ANCESTORS.with(|components| *components.borrow_mut() = ancestors);
    }

    fn dirty_component_ancestors(
        dirty: &HashSet<u64>,
        parents: &HashMap<u64, Option<u64>>,
    ) -> HashSet<u64> {
        let mut ancestors = HashSet::new();
        for id in dirty {
            let mut parent = parents.get(id).copied().flatten();
            while let Some(id) = parent {
                if !ancestors.insert(id) {
                    break;
                }
                parent = parents.get(&id).copied().flatten();
            }
        }
        ancestors
    }

    pub fn component_count(component: &Component) -> usize {
        1 + component
            .children
            .iter()
            .map(component_count)
            .sum::<usize>()
    }

    fn render_children(
        value: Value,
        inherited: Option<&Style>,
        scope_id: u64,
        component_depth: usize,
        ancestor_rendered: bool,
        owner_component: Option<u64>,
    ) -> Vec<Component> {
        match value {
            Value::Array(values) => values
                .borrow()
                .iter()
                .cloned()
                .flat_map(|value| {
                    render_children(
                        value,
                        inherited,
                        scope_id,
                        component_depth,
                        ancestor_rendered,
                        owner_component,
                    )
                })
                .collect(),
            Value::Object(_) => {
                let element_type = value.get_property("type");
                if element_type.is_undefined() {
                    return Vec::new();
                }
                let props = value.get_property("props");
                if element_type.is_function() {
                    let id = stable_keyed_id(scope_id, "component", &props).unwrap_or_else(|| {
                        NEXT_AOT_COMPONENT.with(|next| {
                            let id = next.get();
                            next.set(id + 1);
                            id
                        })
                    });
                    let id = super::ComponentId(id);
                    ACTIVE_AOT_COMPONENTS.with(|components| components.borrow_mut().insert(id.0));
                    COMPONENT_PARENTS
                        .with(|parents| parents.borrow_mut().insert(id.0, owner_component));
                    let dirty =
                        DIRTY_AOT_COMPONENTS.with(|components| components.borrow().contains(&id.0));
                    let dirty_descendant =
                        DIRTY_AOT_ANCESTORS.with(|components| components.borrow().contains(&id.0));
                    let inputs_unchanged = COMPONENT_INPUT_CACHE.with(|cache| {
                        cache
                            .borrow()
                            .get(&id.0)
                            .is_some_and(|previous| shallow_equal_props(previous, &props))
                    });
                    if !ancestor_rendered
                        && !dirty
                        && !dirty_descendant
                        && inputs_unchanged
                        && let Some((cached_inherited, cached)) =
                            COMPONENT_OUTPUT_CACHE.with(|cache| cache.borrow().get(&id.0).cloned())
                        && cached_inherited.as_ref() == inherited
                    {
                        preserve_cached_component_subtree(id.0);
                        return cached;
                    }
                    let cached = (!ancestor_rendered && !dirty && inputs_unchanged)
                        .then(|| {
                            COMPONENT_VALUE_CACHE.with(|cache| cache.borrow().get(&id.0).cloned())
                        })
                        .flatten();
                    let component_rendered = cached.is_none();
                    let rendered = if let Some(cached) = cached {
                        cached
                    } else {
                        super::begin_render(id);
                        let rendered = AOT_COMPONENT_DEPTH.with(|depth| {
                            let previous = depth.replace(component_depth);
                            let previous_component =
                                CURRENT_AOT_COMPONENT.with(|current| current.replace(Some(id.0)));
                            let rendered = element_type.call(Value::Undefined, vec![props.clone()]);
                            CURRENT_AOT_COMPONENT.with(|current| current.set(previous_component));
                            depth.set(previous);
                            rendered
                        });
                        super::end_render(id);
                        COMPONENT_INPUT_CACHE
                            .with(|cache| cache.borrow_mut().insert(id.0, props.clone()));
                        COMPONENT_VALUE_CACHE
                            .with(|cache| cache.borrow_mut().insert(id.0, rendered.clone()));
                        rendered
                    };
                    if std::env::var_os("W3COS_AOT_TRACE").is_some() {
                        eprintln!("[w3cos-aot] component {id:?} -> {}", value_shape(&rendered));
                    }
                    let bailed_out = MEMO_BAILOUT_COMPONENTS
                        .with(|components| components.borrow_mut().remove(&id.0));
                    if bailed_out
                        && let Some((cached_inherited, cached)) =
                            COMPONENT_OUTPUT_CACHE.with(|cache| cache.borrow().get(&id.0).cloned())
                        && cached_inherited.as_ref() == inherited
                    {
                        preserve_cached_component_subtree(id.0);
                        return cached;
                    }
                    let components = render_children(
                        rendered,
                        inherited,
                        id.0,
                        component_depth + 1,
                        component_rendered,
                        Some(id.0),
                    );
                    COMPONENT_OUTPUT_CACHE.with(|cache| {
                        cache
                            .borrow_mut()
                            .insert(id.0, (inherited.cloned(), components.clone()));
                    });
                    return components;
                }
                if element_type.to_js_string() == "react.fragment" {
                    return render_children(
                        props.get_property("children"),
                        inherited,
                        scope_id,
                        component_depth,
                        ancestor_rendered,
                        owner_component,
                    );
                }
                let element_type_string = element_type.to_js_string();
                let host_id = stable_keyed_id(scope_id, &element_type_string, &props)
                    .unwrap_or_else(|| next_scoped_host_id(scope_id, &element_type_string));
                let cached_style = (!ancestor_rendered)
                    .then(|| {
                        HOST_STYLE_CACHE.with(|cache| {
                            cache
                                .borrow()
                                .get(&host_id)
                                .filter(|(previous_props, previous_inherited, _)| {
                                    previous_props.strict_eq(&props)
                                        && previous_inherited.as_ref() == inherited
                                })
                                .map(|(_, _, style)| style.clone())
                        })
                    })
                    .flatten();
                let (host, created) = HOST_ELEMENTS.with(|elements| {
                    let mut elements = elements.borrow_mut();
                    if let Some(host) = elements.get(&host_id) {
                        (host.clone(), false)
                    } else {
                        let host = host_element(host_id);
                        elements.insert(host_id, host.clone());
                        (host, true)
                    }
                });
                if cached_style.is_none() {
                    sync_host_properties(&host, &props);
                    host.set_property("tagName", Value::from(element_type_string.to_uppercase()));
                    host.set_property("localName", Value::from(element_type_string.clone()));
                }
                if created {
                    let reference = props.get_property("ref_");
                    if reference.is_function() {
                        reference.call(Value::Undefined, vec![host.clone()]);
                    } else if !reference.is_nullish() {
                        reference.set_property("current", host.clone());
                    }
                }
                let element_type = element_type_string;
                let style = cached_style.unwrap_or_else(|| {
                    let style = style_from_props(&props, &element_type, inherited);
                    HOST_STYLE_CACHE.with(|cache| {
                        cache
                            .borrow_mut()
                            .insert(host_id, (props.clone(), inherited.cloned(), style.clone()));
                    });
                    style
                });
                let children = render_children(
                    props.get_property("children"),
                    Some(&style),
                    host_id,
                    component_depth,
                    ancestor_rendered,
                    owner_component,
                );
                sync_host_children(&host, &children);
                let mut component = match element_type.as_str() {
                    "abbr" | "b" | "code" | "em" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "i" | "p" | "small" | "span" | "strong" => {
                        let text = children
                            .iter()
                            .filter_map(|child| match &child.kind {
                                w3cos_std::component::ComponentKind::Text { content } => {
                                    Some(content.as_str())
                                }
                                _ => None,
                            })
                            .collect::<String>();
                        Component::text(text, style.clone())
                    }
                    "button" => {
                        let label = children
                            .iter()
                            .filter_map(|child| match &child.kind {
                                w3cos_std::component::ComponentKind::Text { content } => {
                                    Some(content.as_str())
                                }
                                _ => None,
                            })
                            .collect::<String>();
                        Component::button(label, style.clone())
                    }
                    "input" | "textarea" => {
                        let value = props.get_property("value");
                        let default_value = props.get_property("defaultValue");
                        let placeholder = props.get_property("placeholder");
                        Component::text_input(
                            if !value.is_nullish() {
                                value.to_js_string()
                            } else if !default_value.is_nullish() {
                                default_value.to_js_string()
                            } else {
                                String::new()
                            },
                            if placeholder.is_nullish() {
                                String::new()
                            } else {
                                placeholder.to_js_string()
                            },
                            style.clone(),
                        )
                    }
                    "img" => {
                        Component::image(props.get_property("src").to_js_string(), style.clone())
                    }
                    _ => Component::boxed(style, children),
                };
                let on_click = props.get_property("onClick");
                if on_click.is_function() {
                    CLICK_LISTENERS.with(|listeners| {
                        listeners.borrow_mut().insert(host_id, on_click);
                    });
                } else {
                    CLICK_LISTENERS.with(|listeners| listeners.borrow_mut().remove(&host_id));
                }
                let on_scroll = props.get_property("onScroll");
                if on_scroll.is_function() {
                    SCROLL_PROP_LISTENERS.with(|listeners| {
                        listeners.borrow_mut().insert(host_id, on_scroll);
                    });
                } else {
                    SCROLL_PROP_LISTENERS.with(|listeners| listeners.borrow_mut().remove(&host_id));
                }
                sync_listener(&FOCUS_LISTENERS, host_id, props.get_property("onFocus"));
                sync_listener(&BLUR_LISTENERS, host_id, props.get_property("onBlur"));
                sync_listener(&KEYDOWN_LISTENERS, host_id, props.get_property("onKeyDown"));
                sync_listener(&KEYUP_LISTENERS, host_id, props.get_property("onKeyUp"));
                sync_listener(&SUBMIT_LISTENERS, host_id, props.get_property("onSubmit"));
                sync_listener(
                    &BEFORE_INPUT_LISTENERS,
                    host_id,
                    props.get_property("onBeforeInput"),
                );
                sync_listener(
                    &COMPOSITION_START_LISTENERS,
                    host_id,
                    props.get_property("onCompositionStart"),
                );
                sync_listener(
                    &COMPOSITION_UPDATE_LISTENERS,
                    host_id,
                    props.get_property("onCompositionUpdate"),
                );
                sync_listener(
                    &COMPOSITION_END_LISTENERS,
                    host_id,
                    props.get_property("onCompositionEnd"),
                );
                sync_listener(
                    &POINTER_DOWN_LISTENERS,
                    host_id,
                    props.get_property("onPointerDown"),
                );
                sync_listener(
                    &POINTER_UP_LISTENERS,
                    host_id,
                    props.get_property("onPointerUp"),
                );
                sync_listener(
                    &POINTER_MOVE_LISTENERS,
                    host_id,
                    props.get_property("onPointerMove"),
                );
                sync_listener(
                    &POINTER_ENTER_LISTENERS,
                    host_id,
                    props.get_property("onPointerEnter"),
                );
                sync_listener(
                    &POINTER_LEAVE_LISTENERS,
                    host_id,
                    props.get_property("onPointerLeave"),
                );
                sync_listener(
                    &POINTER_CANCEL_LISTENERS,
                    host_id,
                    props.get_property("onPointerCancel"),
                );
                sync_listener(
                    &MOUSE_DOWN_LISTENERS,
                    host_id,
                    props.get_property("onMouseDown"),
                );
                sync_listener(
                    &MOUSE_UP_LISTENERS,
                    host_id,
                    props.get_property("onMouseUp"),
                );
                sync_listener(
                    &MOUSE_MOVE_LISTENERS,
                    host_id,
                    props.get_property("onMouseMove"),
                );
                sync_listener(
                    &MOUSE_ENTER_LISTENERS,
                    host_id,
                    props.get_property("onMouseEnter"),
                );
                sync_listener(
                    &MOUSE_LEAVE_LISTENERS,
                    host_id,
                    props.get_property("onMouseLeave"),
                );
                sync_listener(&WHEEL_LISTENERS, host_id, props.get_property("onWheel"));
                if matches!(
                    component.kind,
                    w3cos_std::component::ComponentKind::TextInput { .. }
                ) {
                    let on_input = props.get_property("onInput");
                    let on_change = props.get_property("onChange");
                    if on_input.is_function() {
                        INPUT_LISTENERS
                            .with(|listeners| listeners.borrow_mut().insert(host_id, on_input));
                    } else {
                        INPUT_LISTENERS.with(|listeners| listeners.borrow_mut().remove(&host_id));
                    }
                    if on_change.is_function() {
                        CHANGE_LISTENERS
                            .with(|listeners| listeners.borrow_mut().insert(host_id, on_change));
                    } else {
                        CHANGE_LISTENERS.with(|listeners| listeners.borrow_mut().remove(&host_id));
                    }
                }
                let click =
                    CLICK_LISTENERS.with(|listeners| listeners.borrow().contains_key(&host_id));
                let scroll = SCROLL_LISTENERS
                    .with(|listeners| listeners.borrow().contains_key(&host_id))
                    || SCROLL_PROP_LISTENERS
                        .with(|listeners| listeners.borrow().contains_key(&host_id));
                let input = INPUT_LISTENERS
                    .with(|listeners| listeners.borrow().contains_key(&host_id))
                    || CHANGE_LISTENERS.with(|listeners| listeners.borrow().contains_key(&host_id))
                    || has_listener(&BEFORE_INPUT_LISTENERS, host_id)
                    || has_listener(&COMPOSITION_START_LISTENERS, host_id)
                    || has_listener(&COMPOSITION_UPDATE_LISTENERS, host_id)
                    || has_listener(&COMPOSITION_END_LISTENERS, host_id);
                let focus = FOCUS_LISTENERS
                    .with(|listeners| listeners.borrow().contains_key(&host_id))
                    || BLUR_LISTENERS.with(|listeners| listeners.borrow().contains_key(&host_id));
                let keyboard = KEYDOWN_LISTENERS
                    .with(|listeners| listeners.borrow().contains_key(&host_id))
                    || KEYUP_LISTENERS.with(|listeners| listeners.borrow().contains_key(&host_id));
                let submit =
                    SUBMIT_LISTENERS.with(|listeners| listeners.borrow().contains_key(&host_id));
                let pointer = has_listener(&POINTER_DOWN_LISTENERS, host_id)
                    || has_listener(&POINTER_UP_LISTENERS, host_id)
                    || has_listener(&POINTER_MOVE_LISTENERS, host_id)
                    || has_listener(&POINTER_ENTER_LISTENERS, host_id)
                    || has_listener(&POINTER_LEAVE_LISTENERS, host_id)
                    || has_listener(&POINTER_CANCEL_LISTENERS, host_id)
                    || has_listener(&MOUSE_DOWN_LISTENERS, host_id)
                    || has_listener(&MOUSE_UP_LISTENERS, host_id)
                    || has_listener(&MOUSE_MOVE_LISTENERS, host_id)
                    || has_listener(&MOUSE_ENTER_LISTENERS, host_id)
                    || has_listener(&MOUSE_LEAVE_LISTENERS, host_id);
                let wheel = has_listener(&WHEEL_LISTENERS, host_id);
                component.on_click = w3cos_std::EventAction::NativeHost {
                    id: host_id,
                    click,
                    scroll,
                    input,
                    focus,
                    keyboard,
                    submit,
                    pointer,
                    wheel,
                };
                vec![component]
            }
            Value::Function(function) => render_children(
                function.call(Value::Undefined, Vec::new()),
                inherited,
                scope_id,
                component_depth,
                ancestor_rendered,
                owner_component,
            ),
            Value::String(text) => vec![Component::text(
                text,
                inherited.map(inherited_text_style).unwrap_or_default(),
            )],
            Value::Number(number) => vec![Component::text(
                Value::Number(number).to_js_string(),
                inherited.map(inherited_text_style).unwrap_or_default(),
            )],
            Value::Bool(_) | Value::Undefined | Value::Null => Vec::new(),
        }
    }

    fn preserve_cached_component_subtree(root_id: u64) {
        let parents = COMPONENT_PARENTS.with(|parents| parents.borrow().clone());
        let previous = LAST_AOT_COMPONENTS.with(|components| components.borrow().clone());
        ACTIVE_AOT_COMPONENTS.with(|active| {
            let mut active = active.borrow_mut();
            for component_id in previous {
                let mut current = Some(component_id);
                while let Some(id) = current {
                    if id == root_id {
                        active.insert(component_id);
                        break;
                    }
                    current = parents.get(&id).copied().flatten();
                }
            }
        });
    }

    fn stable_keyed_id(scope_id: u64, element_type: &str, props: &Value) -> Option<u64> {
        let key = props.get_property("key");
        (!key.is_nullish()).then(|| {
            let mut hasher = DefaultHasher::new();
            scope_id.hash(&mut hasher);
            element_type.hash(&mut hasher);
            key.to_js_string().hash(&mut hasher);
            hasher.finish()
        })
    }

    fn next_scoped_host_id(scope_id: u64, element_type: &str) -> u64 {
        let ordinal = NEXT_HOST_ORDINALS.with(|ordinals| {
            let mut ordinals = ordinals.borrow_mut();
            let ordinal = ordinals.entry(scope_id).or_default();
            let current = *ordinal;
            *ordinal += 1;
            current
        });
        let mut hasher = DefaultHasher::new();
        // Keep unkeyed Host identity local to its owning component/Host scope,
        // matching React's positional reconciliation. A global preorder id
        // lets a removed virtual row donate its DOM identity (and observers)
        // to an unrelated spacer that later occupies the same traversal slot.
        0x5743_4f53_484f_5354_u64.hash(&mut hasher);
        scope_id.hash(&mut hasher);
        element_type.hash(&mut hasher);
        ordinal.hash(&mut hasher);
        hasher.finish()
    }

    fn value_shape(value: &Value) -> String {
        match value {
            Value::Array(values) => format!("array({})", values.borrow().len()),
            Value::Object(_) => {
                let children = value.get_property("props").get_property("children");
                format!("element children={}", value_shape(&children))
            }
            _ => value.type_of().to_string(),
        }
    }

    fn host_element(host_id: u64) -> Value {
        let host = Value::object(std::collections::HashMap::new());
        host.set_property("__w3cosHostId", Value::from(host_id.to_string()));
        host.set_property("scrollTop", Value::Number(0.0));
        host.set_property("scrollLeft", Value::Number(0.0));
        host.set_property("children", Value::array(Vec::new()));
        host.set_property(
            "setAttribute",
            Value::function(move |_, arguments| {
                let Some(name) = arguments.first().map(Value::to_js_string) else {
                    return Value::Undefined;
                };
                let value = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                HOST_ELEMENTS.with(|elements| {
                    if let Some(element) = elements.borrow().get(&host_id) {
                        element.set_property(&name, Value::from(value.to_js_string()));
                    }
                });
                Value::Undefined
            }),
        );
        host.set_property(
            "getAttribute",
            Value::function(move |_, arguments| {
                let Some(name) = arguments.first().map(Value::to_js_string) else {
                    return Value::Null;
                };
                HOST_ELEMENTS.with(|elements| {
                    elements
                        .borrow()
                        .get(&host_id)
                        .map(|element| element.get_property(&name))
                        .filter(|value| !value.is_undefined())
                        .map(|value| Value::from(value.to_js_string()))
                        .unwrap_or(Value::Null)
                })
            }),
        );
        host.set_property(
            "hasAttribute",
            Value::function(move |_, arguments| {
                let Some(name) = arguments.first().map(Value::to_js_string) else {
                    return Value::Bool(false);
                };
                HOST_ELEMENTS.with(|elements| {
                    Value::Bool(
                        elements
                            .borrow()
                            .get(&host_id)
                            .is_some_and(|element| !element.get_property(&name).is_undefined()),
                    )
                })
            }),
        );
        host.set_property(
            "scrollTo",
            Value::function(move |_, arguments| {
                let first = arguments.first().cloned().unwrap_or(Value::Undefined);
                if std::env::var_os("W3COS_AOT_TRACE").is_some() {
                    let top = first.get_property("top");
                    eprintln!(
                        "[w3cos-aot] host {host_id} scrollTo top={} type={} number={}",
                        top.to_js_string(),
                        top.type_of(),
                        top.to_number()
                    );
                }
                let (left, top) = if matches!(first, Value::Object(_)) {
                    let left = first.get_property("left").to_number();
                    let top = first.get_property("top").to_number();
                    (
                        left.is_finite().then_some(left as f32),
                        top.is_finite().then_some(top as f32),
                    )
                } else {
                    let left = first.to_number();
                    let top = arguments
                        .get(1)
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_number();
                    (
                        left.is_finite().then_some(left as f32),
                        top.is_finite().then_some(top as f32),
                    )
                };
                SCROLL_REQUESTS.with(|requests| {
                    requests.borrow_mut().insert(host_id, (left, top));
                });
                Value::Undefined
            }),
        );
        host.set_property(
            "scrollBy",
            Value::function(move |_, arguments| {
                let first = arguments.first().cloned().unwrap_or(Value::Undefined);
                let (delta_left, delta_top) = if matches!(first, Value::Object(_)) {
                    (
                        first.get_property("left").to_number(),
                        first.get_property("top").to_number(),
                    )
                } else {
                    (
                        first.to_number(),
                        arguments
                            .get(1)
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_number(),
                    )
                };
                let (current_left, current_top) = HOST_ELEMENTS.with(|elements| {
                    elements
                        .borrow()
                        .get(&host_id)
                        .map(|element| {
                            (
                                element.get_property("scrollLeft").to_number() as f32,
                                element.get_property("scrollTop").to_number() as f32,
                            )
                        })
                        .unwrap_or_default()
                });
                let left = delta_left
                    .is_finite()
                    .then_some(current_left + delta_left as f32);
                let top = delta_top
                    .is_finite()
                    .then_some(current_top + delta_top as f32);
                SCROLL_REQUESTS.with(|requests| {
                    requests.borrow_mut().insert(host_id, (left, top));
                });
                Value::Undefined
            }),
        );
        host.set_property(
            "addEventListener",
            Value::function(move |_, arguments| {
                if arguments
                    .first()
                    .is_some_and(|event| event.to_js_string() == "scroll")
                {
                    let callback = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                    SCROLL_LISTENERS
                        .with(|listeners| listeners.borrow_mut().insert(host_id, callback));
                }
                Value::Undefined
            }),
        );
        host.set_property(
            "removeEventListener",
            Value::function(move |_, arguments| {
                if arguments
                    .first()
                    .is_some_and(|event| event.to_js_string() == "scroll")
                {
                    SCROLL_LISTENERS.with(|listeners| listeners.borrow_mut().remove(&host_id));
                }
                Value::Undefined
            }),
        );
        host
    }

    fn sync_host_children(host: &Value, children: &[Component]) {
        let child_hosts: Vec<Value> = HOST_ELEMENTS.with(|elements| {
            let elements = elements.borrow();
            children
                .iter()
                .filter_map(|child| match child.on_click {
                    w3cos_std::EventAction::NativeHost { id, .. } => elements.get(&id).cloned(),
                    _ => None,
                })
                .collect()
        });
        if std::env::var_os("W3COS_RESIZE_TRACE").is_some()
            && host.get_property("role").to_js_string() == "list"
        {
            eprintln!(
                "[W3C OS][RESIZE] list host={} component-children={} host-children={}",
                host.get_property("__w3cosHostId").to_js_string(),
                children.len(),
                child_hosts.len()
            );
        }
        host.set_property("children", Value::array(child_hosts));
    }

    fn sync_host_properties(host: &Value, props: &Value) {
        let Value::Object(object) = props else {
            return;
        };
        let keys = object.borrow().keys();
        for key in keys {
            if key == "children"
                || key == "style"
                || key == "ref"
                || key == "ref_"
                || key.starts_with("on")
            {
                continue;
            }
            let value = props.get_property(&key);
            host.set_property(&key, value.clone());
            if key == "className" {
                host.set_property("class", value);
            }
        }
    }

    fn sync_listener(
        listeners: &'static std::thread::LocalKey<
            std::cell::RefCell<std::collections::HashMap<u64, Value>>,
        >,
        host_id: u64,
        listener: Value,
    ) {
        listeners.with(|listeners| {
            let mut listeners = listeners.borrow_mut();
            if listener.is_function() {
                listeners.insert(host_id, listener);
            } else {
                listeners.remove(&host_id);
            }
        });
    }

    fn has_listener(
        listeners: &'static std::thread::LocalKey<
            std::cell::RefCell<std::collections::HashMap<u64, Value>>,
        >,
        host_id: u64,
    ) -> bool {
        listeners.with(|listeners| listeners.borrow().contains_key(&host_id))
    }

    fn style_from_props(props: &Value, element_type: &str, inherited: Option<&Style>) -> Style {
        let source = props.get_property("style");
        let mut style = Style::default();
        style.color = Color::BLACK;
        if let Some(parent) = inherited {
            apply_inherited_text_style(&mut style, parent);
        }
        apply_user_agent_style(&mut style, element_type);
        style.width = dimension(&source.get_property("width"));
        style.height = dimension(&source.get_property("height"));
        style.max_width = dimension(&source.get_property("maxWidth"));
        style.max_height = dimension(&source.get_property("maxHeight"));
        style.min_width = dimension(&source.get_property("minWidth"));
        style.min_height = dimension(&source.get_property("minHeight"));
        style.top = dimension(&source.get_property("top"));
        style.right = dimension(&source.get_property("right"));
        style.bottom = dimension(&source.get_property("bottom"));
        style.left = dimension(&source.get_property("left"));
        let z_index = source.get_property("zIndex").to_number();
        if z_index.is_finite() {
            style.z_index = z_index as i32;
        }
        style.flex_grow = source.get_property("flexGrow").to_number().max(0.0) as f32;
        let flex_shrink = source.get_property("flexShrink").to_number();
        if flex_shrink.is_finite() {
            style.flex_shrink = flex_shrink.max(0.0) as f32;
        }
        style.flex_direction = match source.get_property("flexDirection").to_js_string().as_str() {
            "row-reverse" => FlexDirection::RowReverse,
            "column" => FlexDirection::Column,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => FlexDirection::Row,
        };
        style.flex_wrap = match source.get_property("flexWrap").to_js_string().as_str() {
            "wrap" => FlexWrap::Wrap,
            "wrap-reverse" => FlexWrap::WrapReverse,
            _ => FlexWrap::NoWrap,
        };
        style.align_self = match source.get_property("alignSelf").to_js_string().as_str() {
            "stretch" => AlignSelf::Stretch,
            "center" => AlignSelf::Center,
            "flex-start" => AlignSelf::FlexStart,
            "flex-end" => AlignSelf::FlexEnd,
            "baseline" => AlignSelf::Baseline,
            _ => AlignSelf::Auto,
        };
        style.align_content = match source.get_property("alignContent").to_js_string().as_str() {
            "flex-start" => AlignContent::FlexStart,
            "flex-end" => AlignContent::FlexEnd,
            "center" => AlignContent::Center,
            "space-between" => AlignContent::SpaceBetween,
            "space-around" => AlignContent::SpaceAround,
            "space-evenly" => AlignContent::SpaceEvenly,
            _ => AlignContent::Stretch,
        };
        style.flex_basis = dimension(&source.get_property("flexBasis"));
        let order = source.get_property("order").to_number();
        if order.is_finite() {
            style.order = order as i32;
        }
        style.justify_content = match source
            .get_property("justifyContent")
            .to_js_string()
            .as_str()
        {
            "center" => JustifyContent::Center,
            "flex-end" => JustifyContent::FlexEnd,
            "space-between" => JustifyContent::SpaceBetween,
            "space-around" => JustifyContent::SpaceAround,
            "space-evenly" => JustifyContent::SpaceEvenly,
            _ => JustifyContent::FlexStart,
        };
        style.align_items = match source.get_property("alignItems").to_js_string().as_str() {
            "center" => AlignItems::Center,
            "flex-start" => AlignItems::FlexStart,
            "flex-end" => AlignItems::FlexEnd,
            "baseline" => AlignItems::Baseline,
            _ => AlignItems::Stretch,
        };
        style.gap = source.get_property("gap").to_number().max(0.0) as f32;
        if let Some(padding) = plain_spacing(&source.get_property("padding")) {
            style.padding = Edges::all(padding);
        }
        if let Some(value) = spacing(&source.get_property("paddingTop"), "top") {
            style.padding.top = value;
        }
        if let Some(value) = spacing(&source.get_property("paddingRight"), "right") {
            style.padding.right = value;
        }
        if let Some(value) = spacing(&source.get_property("paddingBottom"), "bottom") {
            style.padding.bottom = value;
        }
        if let Some(value) = spacing(&source.get_property("paddingLeft"), "left") {
            style.padding.left = value;
        }
        if let Some(margin) = plain_spacing(&source.get_property("margin")) {
            style.margin = Edges::all(margin);
        }
        if let Some(value) = spacing(&source.get_property("marginTop"), "top") {
            style.margin.top = value;
        }
        if let Some(value) = spacing(&source.get_property("marginRight"), "right") {
            style.margin.right = value;
        }
        if let Some(value) = spacing(&source.get_property("marginBottom"), "bottom") {
            style.margin.bottom = value;
        }
        if let Some(value) = spacing(&source.get_property("marginLeft"), "left") {
            style.margin.left = value;
        }
        style.font_size = source
            .get_property("fontSize")
            .to_number()
            .is_finite()
            .then(|| source.get_property("fontSize").to_number() as f32)
            .unwrap_or(style.font_size);
        let font_weight = source.get_property("fontWeight").to_number();
        if font_weight.is_finite() {
            style.font_weight = font_weight.max(1.0) as u16;
        }
        let line_height = source.get_property("lineHeight");
        let line_height_number = line_height.to_number();
        if line_height_number.is_finite() {
            style.line_height = line_height_number.max(0.0) as f32;
        } else if let Some(px) = line_height
            .to_js_string()
            .strip_suffix("px")
            .and_then(|value| value.parse::<f32>().ok())
        {
            style.line_height = (px / style.font_size.max(1.0)).max(0.0);
        }
        if let Some(letter_spacing) = plain_spacing(&source.get_property("letterSpacing")) {
            style.letter_spacing = letter_spacing;
        }
        let text_align = source.get_property("textAlign").to_js_string();
        if text_align != "undefined" {
            style.text_align = match text_align.as_str() {
                "center" => TextAlign::Center,
                "right" | "end" => TextAlign::Right,
                "justify" => TextAlign::Justify,
                _ => TextAlign::Left,
            };
        }
        let white_space = source.get_property("whiteSpace").to_js_string();
        if white_space != "undefined" {
            style.white_space = match white_space.as_str() {
                "nowrap" => WhiteSpace::NoWrap,
                "pre" => WhiteSpace::Pre,
                "pre-wrap" => WhiteSpace::PreWrap,
                "pre-line" => WhiteSpace::PreLine,
                _ => WhiteSpace::Normal,
            };
        }
        let text_decoration = source.get_property("textDecoration").to_js_string();
        if text_decoration != "undefined" {
            style.text_decoration = match text_decoration.as_str() {
                "underline" => TextDecoration::Underline,
                "line-through" => TextDecoration::LineThrough,
                "overline" => TextDecoration::Overline,
                _ => TextDecoration::None,
            };
        }
        style.text_overflow = if source.get_property("textOverflow").to_js_string() == "ellipsis" {
            TextOverflow::Ellipsis
        } else {
            TextOverflow::Clip
        };
        let font_family = source.get_property("fontFamily").to_js_string();
        if !matches!(font_family.as_str(), "" | "undefined") {
            style.font_family = Some(font_family);
        }
        let font_style = source.get_property("fontStyle").to_js_string();
        if font_style != "undefined" {
            style.font_style = match font_style.as_str() {
                "italic" => FontStyle::Italic,
                "oblique" => FontStyle::Oblique,
                _ => FontStyle::Normal,
            };
        }
        let word_break = source.get_property("wordBreak").to_js_string();
        if word_break != "undefined" {
            style.word_break = match word_break.as_str() {
                "break-all" => WordBreak::BreakAll,
                "break-word" => WordBreak::BreakWord,
                "keep-all" => WordBreak::KeepAll,
                _ => WordBreak::Normal,
            };
        }
        if let Some(color) = css_color(&source.get_property("color")) {
            style.color = color;
        }
        if let Some(background) = css_color(&source.get_property("backgroundColor"))
            .or_else(|| css_color(&source.get_property("background")))
        {
            style.background = background;
        }
        let border_radius = source.get_property("borderRadius").to_number();
        if border_radius.is_finite() {
            style.border_radius = border_radius.max(0.0) as f32;
        }
        let border_width = source.get_property("borderWidth").to_number();
        if border_width.is_finite() {
            style.border_width = border_width.max(0.0) as f32;
        }
        if let Some(border_color) = css_color(&source.get_property("borderColor")) {
            style.border_color = border_color;
        }
        style.box_shadow = box_shadow(&source.get_property("boxShadow"));
        style.position = match source.get_property("position").to_js_string().as_str() {
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            "sticky" => Position::Sticky,
            "relative" => Position::Relative,
            _ => Position::Static,
        };
        style.display = match source.get_property("display").to_js_string().as_str() {
            "none" => Display::None,
            "block" => Display::Block,
            "inline" => Display::Inline,
            "inline-block" => Display::InlineBlock,
            "grid" => Display::Grid,
            "flex" => Display::Flex,
            _ => match element_type {
                "a" | "abbr" | "b" | "code" | "em" | "i" | "img" | "small" | "span" | "strong" => {
                    Display::Inline
                }
                "button" | "input" | "select" | "textarea" => Display::InlineBlock,
                _ => Display::Block,
            },
        };
        style.overflow = match source.get_property("overflowY").to_js_string().as_str() {
            "auto" => Overflow::Auto,
            "scroll" => Overflow::Scroll,
            "hidden" => Overflow::Hidden,
            _ => Overflow::Visible,
        };
        if matches!(style.overflow, Overflow::Visible) {
            style.overflow = match source.get_property("overflow").to_js_string().as_str() {
                "auto" => Overflow::Auto,
                "scroll" => Overflow::Scroll,
                "hidden" => Overflow::Hidden,
                _ => Overflow::Visible,
            };
        }
        let opacity = source.get_property("opacity").to_number();
        if opacity.is_finite() {
            style.opacity = opacity.clamp(0.0, 1.0) as f32;
        }
        style.pointer_events = if source.get_property("pointerEvents").to_js_string() == "none" {
            PointerEvents::None
        } else {
            PointerEvents::Auto
        };
        let visibility = source.get_property("visibility").to_js_string();
        if visibility != "undefined" {
            style.visibility = match visibility.as_str() {
                "hidden" => Visibility::Hidden,
                "collapse" => Visibility::Collapse,
                _ => Visibility::Visible,
            };
        }
        style.user_select = match source.get_property("userSelect").to_js_string().as_str() {
            "none" => UserSelect::None,
            "text" => UserSelect::Text,
            "all" => UserSelect::All,
            _ => UserSelect::Auto,
        };
        let cursor = source.get_property("cursor").to_js_string();
        if cursor != "undefined" {
            style.cursor = match cursor.as_str() {
                "pointer" => Cursor::Pointer,
                "text" => Cursor::Text,
                "move" => Cursor::Move,
                "grab" => Cursor::Grab,
                "grabbing" => Cursor::Grabbing,
                "not-allowed" => Cursor::NotAllowed,
                "none" => Cursor::None,
                _ => Cursor::Default,
            };
        }
        if let Some(width) = plain_spacing(&source.get_property("outlineWidth")) {
            style.outline_width = width.max(0.0);
        }
        if let Some(color) = css_color(&source.get_property("outlineColor")) {
            style.outline_color = color;
        }
        style.outline_style = match source.get_property("outlineStyle").to_js_string().as_str() {
            "solid" => OutlineStyle::Solid,
            "dashed" => OutlineStyle::Dashed,
            "dotted" => OutlineStyle::Dotted,
            "double" => OutlineStyle::Double,
            _ => OutlineStyle::None,
        };
        style.will_change = WillChange::from_css(&source.get_property("willChange").to_js_string());
        style.overflow_anchor = source
            .get_property("overflowAnchor")
            .to_js_string()
            .as_str()
            != "none";
        style.transition = transition(&source);
        let transform = source.get_property("transform").to_js_string();
        if let Some(value) = transform
            .strip_prefix("translateY(")
            .and_then(|value| value.strip_suffix("px)"))
        {
            let translate_y = value.parse().unwrap_or(0.0);
            if matches!(style.position, Position::Absolute) && matches!(style.top, Dimension::Auto)
            {
                // react-window positions rows with an absolute box plus
                // translateY. The native layout tree paints descendants as
                // independent nodes, so keeping this as a paint-only transform
                // would leave every row's text at y=0. Fold the translation
                // into the absolute layout offset so the whole subtree moves.
                style.top = Dimension::Px(translate_y);
            } else {
                style.transform = Transform2D {
                    translate_y,
                    ..Transform2D::IDENTITY
                };
            }
        } else if let Some(value) = transform
            .strip_prefix("translateX(")
            .and_then(|value| value.strip_suffix("px)"))
        {
            style.transform.translate_x = value.parse().unwrap_or(0.0);
        } else if transform != "undefined" && transform != "none" {
            style.transform = transform_list(&transform);
        }
        style
    }

    fn inherited_text_style(parent: &Style) -> Style {
        let mut style = Style::default();
        apply_inherited_text_style(&mut style, parent);
        style
    }

    fn apply_inherited_text_style(style: &mut Style, parent: &Style) {
        style.color = parent.color;
        style.font_size = parent.font_size;
        style.font_weight = parent.font_weight;
        style.text_align = parent.text_align;
        style.white_space = parent.white_space;
        style.line_height = parent.line_height;
        style.letter_spacing = parent.letter_spacing;
        style.text_decoration = parent.text_decoration;
        style.font_family = parent.font_family.clone();
        style.font_style = parent.font_style;
        style.word_break = parent.word_break;
        style.cursor = parent.cursor;
        style.visibility = parent.visibility;
    }

    fn apply_user_agent_style(style: &mut Style, element_type: &str) {
        let vertical_margin = |style: &mut Style, value: f32| {
            style.margin.top = Spacing::Px(value);
            style.margin.bottom = Spacing::Px(value);
        };
        match element_type {
            "h1" => {
                style.font_size *= 2.0;
                style.font_weight = 700;
                vertical_margin(style, style.font_size * 0.67);
            }
            "h2" => {
                style.font_size *= 1.5;
                style.font_weight = 700;
                vertical_margin(style, style.font_size * 0.83);
            }
            "h3" => {
                style.font_size *= 1.17;
                style.font_weight = 700;
                vertical_margin(style, style.font_size);
            }
            "p" => vertical_margin(style, style.font_size),
            "b" | "strong" => style.font_weight = 700,
            "em" | "i" => style.font_style = FontStyle::Italic,
            _ => {}
        }
    }

    fn transform_list(css: &str) -> Transform2D {
        let mut transform = Transform2D::IDENTITY;
        for item in css.split(')') {
            let item = item.trim();
            let Some((name, raw_args)) = item.split_once('(') else {
                continue;
            };
            let args: Vec<f32> = raw_args
                .split(',')
                .filter_map(|value| {
                    value
                        .trim()
                        .trim_end_matches("px")
                        .trim_end_matches("deg")
                        .parse()
                        .ok()
                })
                .collect();
            match (name.trim(), args.as_slice()) {
                ("translate", [x, y, ..]) => {
                    transform.translate_x = *x;
                    transform.translate_y = *y;
                }
                ("translate", [x]) | ("translateX", [x]) => transform.translate_x = *x,
                ("translateY", [y]) => transform.translate_y = *y,
                ("scale", [xy]) => {
                    transform.scale_x = *xy;
                    transform.scale_y = *xy;
                }
                ("scale", [x, y, ..]) => {
                    transform.scale_x = *x;
                    transform.scale_y = *y;
                }
                ("scaleX", [x]) => transform.scale_x = *x,
                ("scaleY", [y]) => transform.scale_y = *y,
                ("rotate", [degrees]) => transform.rotate_deg = *degrees,
                _ => {}
            }
        }
        transform
    }

    fn transition(source: &Value) -> Option<Transition> {
        let shorthand = source.get_property("transition").to_js_string();
        let mut property = source.get_property("transitionProperty").to_js_string();
        let mut duration = source.get_property("transitionDuration").to_js_string();
        let mut delay = source.get_property("transitionDelay").to_js_string();
        let mut easing = source
            .get_property("transitionTimingFunction")
            .to_js_string();
        if !shorthand.is_empty() && shorthand != "undefined" {
            let mut saw_duration = false;
            for part in shorthand.split_whitespace() {
                if part.ends_with("ms") || part.ends_with('s') {
                    if saw_duration {
                        delay = part.to_string();
                    } else {
                        duration = part.to_string();
                        saw_duration = true;
                    }
                } else if matches!(
                    part,
                    "ease" | "linear" | "ease-in" | "ease-out" | "ease-in-out"
                ) {
                    easing = part.to_string();
                } else {
                    property = part.to_string();
                }
            }
        }
        let duration_ms = if let Some(value) = duration.strip_suffix("ms") {
            value.parse().ok()
        } else if let Some(value) = duration.strip_suffix('s') {
            value
                .parse::<f32>()
                .ok()
                .map(|seconds| (seconds * 1000.0) as u32)
        } else {
            let value = source.get_property("transitionDuration").to_number();
            value.is_finite().then_some(value.max(0.0) as u32)
        }?;
        let parse_time = |value: &str| {
            if let Some(value) = value.strip_suffix("ms") {
                value.parse::<f32>().ok().map(|value| value.max(0.0) as u32)
            } else if let Some(value) = value.strip_suffix('s') {
                value
                    .parse::<f32>()
                    .ok()
                    .map(|seconds| (seconds.max(0.0) * 1000.0) as u32)
            } else {
                None
            }
        };
        Some(Transition {
            property: match property.as_str() {
                "opacity" => TransitionProperty::Opacity,
                "transform" => TransitionProperty::Transform,
                "background" | "background-color" => TransitionProperty::Background,
                "color" => TransitionProperty::Color,
                "" | "undefined" | "all" => TransitionProperty::All,
                custom => TransitionProperty::Custom(custom.to_string()),
            },
            duration_ms,
            easing: match easing.as_str() {
                "linear" => Easing::Linear,
                "ease-in" => Easing::EaseIn,
                "ease-out" => Easing::EaseOut,
                "ease-in-out" => Easing::EaseInOut,
                _ => Easing::Ease,
            },
            delay_ms: parse_time(&delay).unwrap_or(0),
        })
    }

    fn css_color(value: &Value) -> Option<Color> {
        Color::from_css(&value.to_js_string())
    }

    fn box_shadow(value: &Value) -> Option<BoxShadow> {
        let css = value.to_js_string();
        if css.is_empty() || css == "undefined" || css == "none" {
            return None;
        }
        let mut lengths = Vec::new();
        let mut color = None;
        let mut inset = false;
        for token in css.split_whitespace() {
            if token == "inset" {
                inset = true;
            } else if token.starts_with('#') {
                color = Some(Color::from_hex(token));
            } else if let Some(value) = token.strip_suffix("px") {
                lengths.push(value.parse::<f32>().ok()?);
            } else if let Ok(value) = token.parse::<f32>() {
                lengths.push(value);
            }
        }
        if lengths.len() < 2 {
            return None;
        }
        Some(BoxShadow {
            offset_x: lengths[0],
            offset_y: lengths[1],
            blur_radius: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
            spread_radius: lengths.get(3).copied().unwrap_or(0.0),
            color: color.unwrap_or(Color::rgba(0, 0, 0, 64)),
            inset,
        })
    }

    fn plain_spacing(value: &Value) -> Option<f32> {
        let number = value.to_number();
        if number.is_finite() {
            return Some(number as f32);
        }
        value
            .to_js_string()
            .strip_suffix("px")
            .and_then(|value| value.trim().parse().ok())
    }

    fn spacing(value: &Value, edge: &str) -> Option<Spacing> {
        let number = value.to_number();
        if number.is_finite() {
            return Some(Spacing::Px(number as f32));
        }
        let css = value.to_js_string();
        let safe_area = match edge {
            "top" => w3cos_std::safe_area::SafeAreaEdge::Top,
            "right" => w3cos_std::safe_area::SafeAreaEdge::Right,
            "bottom" => w3cos_std::safe_area::SafeAreaEdge::Bottom,
            "left" => w3cos_std::safe_area::SafeAreaEdge::Left,
            _ => return None,
        };
        if css == format!("env(safe-area-inset-{edge})") {
            return Some(Spacing::SafeAreaInset(safe_area));
        }
        let env = format!("env(safe-area-inset-{edge})");
        if let Some(inner) = css
            .strip_prefix("calc(")
            .and_then(|css| css.strip_suffix(')'))
            && let Some(px) = inner
                .replace(&env, "")
                .replace('+', "")
                .trim()
                .strip_suffix("px")
                .and_then(|value| value.trim().parse().ok())
        {
            return Some(Spacing::Composite {
                px,
                safe_area: Some(safe_area),
                keyboard_inset: false,
            });
        }
        css.strip_suffix("px")
            .and_then(|value| value.parse().ok())
            .map(Spacing::Px)
    }

    fn dimension(value: &Value) -> Dimension {
        match value {
            Value::Number(number) if number.is_finite() => Dimension::Px(*number as f32),
            Value::String(value) if value.ends_with('%') => value[..value.len() - 1]
                .parse()
                .map(Dimension::Percent)
                .unwrap_or(Dimension::Auto),
            Value::String(value) if value.ends_with("px") => value[..value.len() - 2]
                .parse()
                .map(Dimension::Px)
                .unwrap_or(Dimension::Auto),
            Value::String(value) if value.ends_with("rem") => value[..value.len() - 3]
                .parse()
                .map(Dimension::Rem)
                .unwrap_or(Dimension::Auto),
            Value::String(value) if value.ends_with("em") => value[..value.len() - 2]
                .parse()
                .map(Dimension::Em)
                .unwrap_or(Dimension::Auto),
            Value::String(value) if value.ends_with("vw") => value[..value.len() - 2]
                .parse()
                .map(Dimension::Vw)
                .unwrap_or(Dimension::Auto),
            Value::String(value) if value.ends_with("vh") => value[..value.len() - 2]
                .parse()
                .map(Dimension::Vh)
                .unwrap_or(Dimension::Auto),
            _ => Dimension::Auto,
        }
    }

    pub fn useState(initial: Value) -> Value {
        let state = use_state(move || {
            if initial.is_function() {
                initial.call(Value::Undefined, vec![])
            } else {
                initial
            }
        });
        let current = state.get();
        if std::env::var_os("W3COS_AOT_TRACE").is_some() {
            eprintln!(
                "[w3cos-aot] useState {} range=({}, {})",
                value_shape(&current),
                current.get_property("startIndexOverscan").to_js_string(),
                current.get_property("stopIndexOverscan").to_js_string()
            );
        }
        let setter_state = state.clone();
        let setter = Value::function(move |_, arguments| {
            let next = arguments.first().cloned().unwrap_or(Value::Undefined);
            let next = if next.is_function() {
                next.call(Value::Undefined, vec![setter_state.get()])
            } else {
                next
            };
            setter_state.set(next);
            Value::Undefined
        });
        Value::array(vec![current, setter])
    }

    pub fn useMemo(factory: Value, dependencies: Value) -> Value {
        let dependencies = deps(&dependencies);
        use_memo(
            move || factory.call(Value::Undefined, vec![]),
            &dependencies,
        )
        .get()
    }

    pub fn useCallback(callback: Value, dependencies: Value) -> Value {
        let dependencies = deps(&dependencies);
        use_memo(move || callback, &dependencies).get()
    }

    pub fn useRef(initial: Value) -> Value {
        let reference = use_ref(|| {
            let object = crate::aot_object();
            object.set_property("current", initial);
            object
        });
        reference.with(Clone::clone)
    }

    pub fn useEffect(effect: Value, dependencies: Value) -> Value {
        let dependencies = deps(&dependencies);
        use_effect(
            move || {
                let cleanup = Rc::new(std::cell::RefCell::new(None::<Value>));
                let effect_cleanup = Rc::clone(&cleanup);
                let depth = AOT_COMPONENT_DEPTH.with(std::cell::Cell::get);
                PENDING_EFFECTS.with(|pending| {
                    pending.borrow_mut().push((
                        depth,
                        Box::new(move || {
                            let next_cleanup = effect.call(Value::Undefined, vec![]);
                            if next_cleanup.is_function() {
                                *effect_cleanup.borrow_mut() = Some(next_cleanup);
                            }
                        }),
                    ));
                });
                Some(Box::new(move || {
                    if let Some(cleanup) = cleanup.borrow_mut().take() {
                        cleanup.call(Value::Undefined, vec![]);
                    }
                }) as Box<dyn FnOnce()>)
            },
            &dependencies,
        );
        Value::Undefined
    }

    pub fn useLayoutEffect(effect: Value, dependencies: Value) -> Value {
        let dependencies = deps(&dependencies);
        use_effect(
            move || {
                let cleanup = Rc::new(std::cell::RefCell::new(None::<Value>));
                let effect_cleanup = Rc::clone(&cleanup);
                let depth = AOT_COMPONENT_DEPTH.with(std::cell::Cell::get);
                PENDING_LAYOUT_EFFECTS.with(|pending| {
                    pending.borrow_mut().push((
                        depth,
                        Box::new(move || {
                            let next_cleanup = effect.call(Value::Undefined, vec![]);
                            if next_cleanup.is_function() {
                                *effect_cleanup.borrow_mut() = Some(next_cleanup);
                            }
                        }),
                    ));
                });
                Some(Box::new(move || {
                    if let Some(cleanup) = cleanup.borrow_mut().take() {
                        cleanup.call(Value::Undefined, vec![]);
                    }
                }) as Box<dyn FnOnce()>)
            },
            &dependencies,
        );
        Value::Undefined
    }

    pub fn memo(component: Value, compare: Value) -> Value {
        Value::function(move |_, arguments| {
            let props = arguments.first().cloned().unwrap_or_else(crate::aot_object);
            let component_id = CURRENT_AOT_COMPONENT.with(std::cell::Cell::get);
            let dirty = component_id.is_some_and(|id| {
                DIRTY_AOT_COMPONENTS.with(|components| components.borrow().contains(&id))
            });
            if let Some(component_id) = component_id
                && !dirty
                && let Some((previous_props, previous_output)) =
                    MEMO_INSTANCES.with(|instances| instances.borrow().get(&component_id).cloned())
            {
                let equal = if compare.is_function() {
                    compare
                        .call(
                            Value::Undefined,
                            vec![previous_props.clone(), props.clone()],
                        )
                        .to_bool()
                } else {
                    shallow_equal_props(&previous_props, &props)
                };
                if equal {
                    MEMO_BAILOUT_COMPONENTS
                        .with(|components| components.borrow_mut().insert(component_id));
                    return previous_output;
                }
            }
            let output = component.call(Value::Undefined, vec![props.clone()]);
            if let Some(component_id) = component_id {
                MEMO_BAILOUT_COMPONENTS
                    .with(|components| components.borrow_mut().remove(&component_id));
                MEMO_INSTANCES.with(|instances| {
                    instances
                        .borrow_mut()
                        .insert(component_id, (props, output.clone()));
                });
            }
            output
        })
    }

    fn shallow_equal_props(previous: &Value, next: &Value) -> bool {
        let (Value::Object(previous), Value::Object(next)) = (previous, next) else {
            return previous.strict_eq(next);
        };
        let mut previous_keys = previous.borrow().keys();
        let mut next_keys = next.borrow().keys();
        previous_keys.sort();
        next_keys.sort();
        previous_keys == next_keys
            && previous_keys.iter().all(|key| {
                previous
                    .borrow()
                    .get_direct(key)
                    .strict_eq(&next.borrow().get_direct(key))
            })
    }

    pub fn createElement(arguments: Vec<Value>) -> Value {
        let element_type = arguments.first().cloned().unwrap_or(Value::Undefined);
        let props = arguments.get(1).cloned().unwrap_or_else(crate::aot_object);
        if arguments.len() > 2 {
            let children = if arguments.len() == 3 {
                arguments[2].clone()
            } else {
                Value::array(arguments[2..].to_vec())
            };
            props.set_property("children", children);
        }
        let element = crate::aot_object();
        element.set_property("type", element_type);
        element.set_property("props", props);
        element
    }

    fn jsx_runtime(arguments: Vec<Value>) -> Value {
        let element_type = arguments.first().cloned().unwrap_or(Value::Undefined);
        let props = arguments.get(1).cloned().unwrap_or_else(crate::aot_object);
        if let Some(key) = arguments.get(2)
            && !key.is_nullish()
        {
            props.set_property("key", key.clone());
        }
        let element = crate::aot_object();
        element.set_property("type", element_type);
        element.set_property("props", props);
        element
    }

    pub fn useImperativeHandle(reference: Value, factory: Value, dependencies: Value) -> Value {
        let value = useMemo(factory, dependencies);
        if reference.is_function() {
            reference.call(Value::Undefined, vec![value]);
        } else {
            reference.set_property("current", value);
        }
        Value::Undefined
    }

    pub fn jsx(element_type: Value, props: Value) -> Value {
        createElement(vec![element_type, props])
    }

    pub fn jsxs(element_type: Value, props: Value) -> Value {
        createElement(vec![element_type, props])
    }

    pub fn Fragment() -> Value {
        Value::from("react.fragment")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        use w3cos_std::EventAction;
        use w3cos_std::component::ComponentKind;

        #[test]
        fn use_state_lazy_initializer_runs_only_for_initial_mount() {
            let component_id = crate::ComponentId(0x5a17_e001);
            let calls = Rc::new(Cell::new(0));
            let initializer = {
                let calls = Rc::clone(&calls);
                Value::function(move |_, _| {
                    calls.set(calls.get() + 1);
                    Value::Number(42.0)
                })
            };

            crate::begin_render(component_id);
            let first = useState(initializer.clone()).get_property("0");
            crate::end_render(component_id);
            crate::begin_render(component_id);
            let second = useState(initializer).get_property("0");
            crate::end_render(component_id);
            crate::unmount(component_id);

            assert_eq!(first.to_number(), 42.0);
            assert_eq!(second.to_number(), 42.0);
            assert_eq!(calls.get(), 1);
        }

        #[test]
        fn memo_uses_comparator_and_reuses_the_retained_host_subtree() {
            let calls = Rc::new(Cell::new(0));
            let component_calls = Rc::clone(&calls);
            let component = Value::function(move |_, arguments| {
                component_calls.set(component_calls.get() + 1);
                let props = arguments.first().cloned().unwrap_or(Value::Undefined);
                let child_props = crate::aot_object();
                child_props.set_property("children", props.get_property("label"));
                createElement(vec![Value::from("span"), child_props])
            });
            let compare = Value::function(|_, arguments| {
                Value::Bool(
                    arguments[0]
                        .get_property("label")
                        .strict_eq(&arguments[1].get_property("label")),
                )
            });
            let memoized = memo(component, compare);
            let render = |label: &str| {
                let props = crate::aot_object();
                props.set_property("key", Value::from("stable-row"));
                props.set_property("label", Value::from(label));
                render_to_component(createElement(vec![memoized.clone(), props]))
            };

            let first = render("unchanged");
            let second = render("unchanged");
            let changed = render("changed");

            assert_eq!(calls.get(), 2);
            let text = |component: &Component| match &component.kind {
                ComponentKind::Text { content } => content.clone(),
                other => panic!("expected text component, got {other:?}"),
            };
            assert_eq!(text(&first), "unchanged");
            assert_eq!(text(&second), "unchanged");
            assert_eq!(text(&changed), "changed");
        }

        #[test]
        fn dirty_component_ancestors_exclude_clean_sibling_subtrees() {
            let dirty = HashSet::from([4]);
            let parents = HashMap::from([
                (1, None),
                (2, Some(1)),
                (3, Some(1)),
                (4, Some(2)),
                (5, Some(3)),
            ]);

            let ancestors = dirty_component_ancestors(&dirty, &parents);

            assert_eq!(ancestors, HashSet::from([1, 2]));
            assert!(!ancestors.contains(&3));
            assert!(!ancestors.contains(&5));
        }

        #[test]
        fn absolute_translate_y_positions_the_entire_host_subtree() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("position", Value::from("absolute"));
            style.set_property("transform", Value::from("translateY(76px)"));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style.clone());

            let native = style_from_props(&props, "div", None);
            assert!(matches!(native.position, Position::Absolute));
            assert!(matches!(native.top, Dimension::Px(value) if value == 76.0));
            assert!(native.transform.is_identity());
        }

        #[test]
        fn positioned_overlay_keeps_transitioned_transform() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("position", Value::from("absolute"));
            style.set_property("top", Value::Number(0.0));
            style.set_property("transform", Value::from("translateY(900px)"));
            style.set_property("transition", Value::from("all 300ms ease-out"));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style);

            let native = style_from_props(&props, "div", None);
            assert!(matches!(native.top, Dimension::Px(0.0)));
            assert_eq!(native.transform.translate_y, 900.0);
            let transition = native.transition.expect("transition should be mapped");
            assert_eq!(transition.duration_ms, 300);
            assert!(matches!(transition.property, TransitionProperty::All));
            assert!(matches!(transition.easing, Easing::EaseOut));
        }

        #[test]
        fn safe_area_environment_padding_is_resolved_by_the_host() {
            w3cos_std::safe_area::set_enabled(true);
            w3cos_std::safe_area::set_insets(w3cos_std::safe_area::SafeAreaInsets {
                top: 59.0,
                ..Default::default()
            });
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("paddingTop", Value::from("env(safe-area-inset-top)"));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style.clone());

            let native = style_from_props(&props, "div", None);
            assert!(matches!(
                native.padding.top,
                Spacing::SafeAreaInset(w3cos_std::safe_area::SafeAreaEdge::Top)
            ));

            style.set_property(
                "paddingTop",
                Value::from("calc(18px + env(safe-area-inset-top))"),
            );
            let native = style_from_props(&props, "div", None);
            assert!(matches!(
                native.padding.top,
                Spacing::Composite {
                    px: 18.0,
                    safe_area: Some(w3cos_std::safe_area::SafeAreaEdge::Top),
                    keyboard_inset: false,
                }
            ));
            w3cos_std::safe_area::set_enabled(false);
        }

        #[test]
        fn native_button_keeps_label_style_and_click_callback() {
            let clicks = Rc::new(Cell::new(0));
            let callback_clicks = Rc::clone(&clicks);
            let props = Value::object(std::collections::HashMap::new());
            props.set_property(
                "onClick",
                Value::function(move |_, _| {
                    callback_clicks.set(callback_clicks.get() + 1);
                    Value::Undefined
                }),
            );
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("height", Value::Number(42.0));
            props.set_property("style", style);

            let element = createElement(vec![Value::from("button"), props, Value::from("打开")]);
            let component = render_to_component(element);

            assert!(matches!(
                component.kind,
                ComponentKind::Button { ref label } if label == "打开"
            ));
            assert!(matches!(component.style.height, Dimension::Px(42.0)));
            let EventAction::NativeHost {
                id: host_id,
                click: true,
                ..
            } = component.on_click
            else {
                panic!("button did not register its native click callback");
            };
            dispatch_click(host_id);
            assert_eq!(clicks.get(), 1);
        }

        #[test]
        fn native_scroll_prop_receives_current_target_offset() {
            let observed = Rc::new(Cell::new(0.0));
            let callback_observed = Rc::clone(&observed);
            let props = Value::object(std::collections::HashMap::new());
            props.set_property(
                "onScroll",
                Value::function(move |_, arguments| {
                    let offset = arguments
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .get_property("currentTarget")
                        .get_property("scrollTop")
                        .to_number();
                    callback_observed.set(offset);
                    Value::Undefined
                }),
            );
            let element = createElement(vec![Value::from("div"), props]);
            let component = render_to_component(element);
            let EventAction::NativeHost {
                id: host_id,
                scroll: true,
                ..
            } = component.on_click
            else {
                panic!("scroll host did not register its onScroll callback");
            };

            dispatch_scroll(host_id, 168.0);

            assert_eq!(observed.get(), 168.0);
        }

        #[test]
        fn host_scroll_to_queues_standard_dom_request() {
            let _ = take_scroll_requests();
            let host = host_element(41);
            let options = Value::object(std::collections::HashMap::new());
            options.set_property("left", Value::Number(12.0));
            options.set_property("top", Value::Number(8_400.0));

            host.get_property("scrollTo")
                .call(host.clone(), vec![options]);

            assert_eq!(
                take_scroll_requests(),
                vec![(41, Some(12.0), Some(8_400.0))]
            );
        }

        #[test]
        fn host_children_and_attributes_match_dom_collection_contract() {
            let parent_host = Rc::new(RefCell::new(Value::Undefined));
            let captured_parent = Rc::clone(&parent_host);
            let parent_props = Value::object(std::collections::HashMap::new());
            parent_props.set_property(
                "ref_",
                Value::function(move |_, arguments| {
                    *captured_parent.borrow_mut() =
                        arguments.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                }),
            );
            let child_props = Value::object(std::collections::HashMap::new());
            child_props.set_property("aria-hidden", Value::Bool(true));
            let child = createElement(vec![Value::from("div"), child_props]);

            let _ =
                render_to_component(createElement(vec![Value::from("div"), parent_props, child]));

            let children = parent_host.borrow().get_property("children");
            assert_eq!(children.get_property("length").to_number(), 1.0);
            let child_host = children.get_property("0");
            assert!(
                child_host
                    .call_method("hasAttribute", vec![Value::from("aria-hidden")])
                    .to_bool()
            );
            child_host.call_method(
                "setAttribute",
                vec![Value::from("data-row-index"), Value::from("24")],
            );
            assert_eq!(
                child_host
                    .call_method("getAttribute", vec![Value::from("data-row-index")],)
                    .to_js_string(),
                "24"
            );
            assert!(!child_host.get_property("__w3cosHostId").is_undefined());
        }

        #[test]
        fn imperative_handle_supports_callback_refs() {
            let observed = Rc::new(Cell::new(0.0));
            let callback_observed = Rc::clone(&observed);
            let reference = Value::function(move |_, arguments| {
                callback_observed.set(
                    arguments
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .get_property("offset")
                        .to_number(),
                );
                Value::Undefined
            });
            let factory = Value::function(|_, _| {
                let handle = Value::object(std::collections::HashMap::new());
                handle.set_property("offset", Value::Number(84.0));
                handle
            });

            super::super::begin_render(super::super::ComponentId(90_041));
            useImperativeHandle(reference, factory, Value::array(Vec::new()));
            super::super::end_render(super::super::ComponentId(90_041));

            assert_eq!(observed.get(), 84.0);
        }

        #[test]
        fn aot_effect_runs_after_host_ref_is_committed() {
            let observed = Rc::new(Cell::new(false));
            let component_observed = Rc::clone(&observed);
            let component = Value::function(move |_, _| {
                let reference = useRef(Value::Null);
                let effect_reference = reference.clone();
                let effect_observed = Rc::clone(&component_observed);
                useEffect(
                    Value::function(move |_, _| {
                        effect_observed.set(
                            effect_reference
                                .get_property("current")
                                .get_property("localName")
                                .to_js_string()
                                == "div",
                        );
                        Value::Undefined
                    }),
                    Value::array(Vec::new()),
                );
                let props = Value::object(std::collections::HashMap::new());
                props.set_property("key", Value::from("effect-ref-commit-test"));
                props.set_property("ref_", reference);
                createElement(vec![Value::from("div"), props])
            });

            let component_props = Value::object(std::collections::HashMap::new());
            component_props.set_property("key", Value::from("effect-component-commit-test"));
            render_to_component(createElement(vec![component, component_props]));

            assert!(observed.get());
        }

        #[test]
        fn aot_layout_effects_flush_before_passive_effects() {
            let order = Rc::new(std::cell::RefCell::new(Vec::new()));
            let component_order = Rc::clone(&order);
            let component = Value::function(move |_, _| {
                let passive_order = Rc::clone(&component_order);
                useEffect(
                    Value::function(move |_, _| {
                        passive_order.borrow_mut().push("passive");
                        Value::Undefined
                    }),
                    Value::array(Vec::new()),
                );
                let layout_order = Rc::clone(&component_order);
                useLayoutEffect(
                    Value::function(move |_, _| {
                        layout_order.borrow_mut().push("layout");
                        Value::Undefined
                    }),
                    Value::array(Vec::new()),
                );
                createElement(vec![
                    Value::from("div"),
                    Value::object(std::collections::HashMap::new()),
                ])
            });
            let component_props = Value::object(std::collections::HashMap::new());
            component_props.set_property("key", Value::from("effect-order-test"));

            render_to_component(createElement(vec![component, component_props]));

            assert_eq!(&*order.borrow(), &["layout", "passive"]);
        }

        #[test]
        fn aot_child_effects_flush_before_parent_effects() {
            let order = Rc::new(std::cell::RefCell::new(Vec::new()));
            let child_order = Rc::clone(&order);
            let child = Value::function(move |_, _| {
                let effect_order = Rc::clone(&child_order);
                useEffect(
                    Value::function(move |_, _| {
                        effect_order.borrow_mut().push("child");
                        Value::Undefined
                    }),
                    Value::array(Vec::new()),
                );
                createElement(vec![
                    Value::from("div"),
                    Value::object(std::collections::HashMap::new()),
                ])
            });
            let parent_order = Rc::clone(&order);
            let parent = Value::function(move |_, _| {
                let effect_order = Rc::clone(&parent_order);
                useEffect(
                    Value::function(move |_, _| {
                        effect_order.borrow_mut().push("parent");
                        Value::Undefined
                    }),
                    Value::array(Vec::new()),
                );
                let child_props = Value::object(std::collections::HashMap::new());
                child_props.set_property("key", Value::from("effect-child-test"));
                createElement(vec![child.clone(), child_props])
            });
            let parent_props = Value::object(std::collections::HashMap::new());
            parent_props.set_property("key", Value::from("effect-parent-test"));

            render_to_component(createElement(vec![parent, parent_props]));

            assert_eq!(&*order.borrow(), &["child", "parent"]);
        }

        #[test]
        fn one_host_keeps_click_and_scroll_capabilities_together() {
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("onClick", Value::function(|_, _| Value::Undefined));
            props.set_property("onScroll", Value::function(|_, _| Value::Undefined));

            let component = render_to_component(createElement(vec![Value::from("div"), props]));

            assert!(matches!(
                component.on_click,
                EventAction::NativeHost {
                    click: true,
                    scroll: true,
                    ..
                }
            ));
        }

        #[test]
        fn explicit_height_host_keeps_web_flex_shrink_initial_value() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("height", Value::Number(84_000.0));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style);

            let native = style_from_props(&props, "div", None);
            assert!(matches!(native.height, Dimension::Px(value) if value == 84_000.0));
            assert_eq!(native.flex_shrink, 1.0);
        }

        #[test]
        fn intrinsic_host_maps_common_box_text_styles() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("marginTop", Value::from("12px"));
            style.set_property("lineHeight", Value::from("24px"));
            style.set_property("fontSize", Value::Number(16.0));
            style.set_property("letterSpacing", Value::Number(0.5));
            style.set_property("textAlign", Value::from("center"));
            style.set_property("whiteSpace", Value::from("nowrap"));
            style.set_property("boxShadow", Value::from("0px 3px 8px #cdd9e8"));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style.clone());

            let native = style_from_props(&props, "div", None);
            assert!(matches!(native.display, Display::Block));
            assert!(matches!(native.flex_direction, FlexDirection::Row));
            assert!(matches!(native.margin.top, Spacing::Px(12.0)));
            assert_eq!(native.line_height, 1.5);
            assert_eq!(native.letter_spacing, 0.5);
            assert!(matches!(native.text_align, TextAlign::Center));
            assert!(matches!(native.white_space, WhiteSpace::NoWrap));
            let shadow = native.box_shadow.expect("box shadow should be mapped");
            assert_eq!(shadow.offset_y, 3.0);
            assert_eq!(shadow.blur_radius, 8.0);

            style.set_property("display", Value::from("flex"));
            let native = style_from_props(&props, "div", None);
            assert!(matches!(native.display, Display::Flex));
            assert!(matches!(native.flex_direction, FlexDirection::Row));

            style.set_property("flexDirection", Value::from("column"));
            assert!(matches!(
                style_from_props(&props, "div", None).flex_direction,
                FlexDirection::Column
            ));
        }

        #[test]
        fn extended_css_host_styles_and_transform_list_are_mapped() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("flexBasis", Value::from("25%"));
            style.set_property("alignContent", Value::from("space-between"));
            style.set_property("textOverflow", Value::from("ellipsis"));
            style.set_property("fontStyle", Value::from("italic"));
            style.set_property("wordBreak", Value::from("break-all"));
            style.set_property("visibility", Value::from("hidden"));
            style.set_property("userSelect", Value::from("none"));
            style.set_property("outlineWidth", Value::from("2px"));
            style.set_property("outlineStyle", Value::from("solid"));
            style.set_property(
                "transform",
                Value::from("translate(8px, 12px) scale(1.2) rotate(5deg)"),
            );
            style.set_property("transition", Value::from("transform 280ms ease-out 40ms"));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style);

            let native = style_from_props(&props, "div", None);
            assert!(matches!(native.flex_basis, Dimension::Percent(25.0)));
            assert!(matches!(native.align_content, AlignContent::SpaceBetween));
            assert!(matches!(native.text_overflow, TextOverflow::Ellipsis));
            assert!(matches!(native.font_style, FontStyle::Italic));
            assert!(matches!(native.word_break, WordBreak::BreakAll));
            assert!(matches!(native.visibility, Visibility::Hidden));
            assert!(matches!(native.user_select, UserSelect::None));
            assert_eq!(native.outline_width, 2.0);
            assert_eq!(native.transform.translate_x, 8.0);
            assert_eq!(native.transform.translate_y, 12.0);
            assert_eq!(native.transform.scale_x, 1.2);
            assert_eq!(native.transform.rotate_deg, 5.0);
            assert_eq!(native.transition.expect("transition").delay_ms, 40);
        }

        #[test]
        fn native_input_preserves_value_and_placeholder() {
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("value", Value::from("上海"));
            props.set_property("placeholder", Value::from("请输入目的地"));
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("flexGrow", Value::from(1));
            props.set_property("style", style);

            let component = render_to_component(createElement(vec![Value::from("input"), props]));

            assert!(matches!(
                component.kind,
                ComponentKind::TextInput { ref value, ref placeholder }
                    if value == "上海" && placeholder == "请输入目的地"
            ));
            assert_eq!(component.style.flex_grow, 1.0);
            assert_eq!(component.style.flex_shrink, 1.0);
        }

        #[test]
        fn native_input_uses_default_value_without_rendering_undefined() {
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("defaultValue", Value::from(""));
            props.set_property("placeholder", Value::from("请输入"));

            let component = render_to_component(createElement(vec![Value::from("input"), props]));

            assert!(matches!(
                component.kind,
                ComponentKind::TextInput { ref value, ref placeholder }
                    if value.is_empty() && placeholder == "请输入"
            ));
        }

        #[test]
        fn react_fragments_flatten_and_empty_values_do_not_create_layout_nodes() {
            let first = createElement(vec![
                Value::from("span"),
                crate::aot_object(),
                Value::from("first"),
            ]);
            let second = createElement(vec![
                Value::from("span"),
                crate::aot_object(),
                Value::from("second"),
            ]);
            let fragment = createElement(vec![
                Fragment(),
                crate::aot_object(),
                Value::array(vec![
                    first,
                    Value::Null,
                    Value::Bool(false),
                    Value::Undefined,
                    second,
                ]),
            ]);
            let parent = createElement(vec![Value::from("div"), crate::aot_object(), fragment]);

            let component = render_to_component(parent);

            assert!(matches!(component.kind, ComponentKind::Box));
            assert_eq!(component.children.len(), 2);
            assert!(matches!(
                component.children[0].kind,
                ComponentKind::Text { ref content } if content == "first"
            ));
            assert!(matches!(
                component.children[1].kind,
                ComponentKind::Text { ref content } if content == "second"
            ));
        }

        #[test]
        fn intrinsic_img_uses_the_native_image_renderer() {
            let props = crate::aot_object();
            props.set_property("src", Value::from("assets/truck.png"));

            let component = render_to_component(createElement(vec![Value::from("img"), props]));

            assert!(matches!(
                component.kind,
                ComponentKind::Image { ref src } if src == "assets/truck.png"
            ));
            assert!(matches!(component.style.display, Display::Inline));
        }

        #[test]
        fn inherited_text_properties_flow_through_intrinsic_hosts() {
            let parent_style = crate::aot_object();
            parent_style.set_property("color", Value::from("#123456"));
            parent_style.set_property("fontSize", Value::Number(22.0));
            parent_style.set_property("fontWeight", Value::Number(700.0));
            let parent_props = crate::aot_object();
            parent_props.set_property("style", parent_style);
            let child = createElement(vec![
                Value::from("span"),
                crate::aot_object(),
                Value::from("inherited"),
            ]);

            let component =
                render_to_component(createElement(vec![Value::from("div"), parent_props, child]));

            let child = &component.children[0];
            assert_eq!(child.style.color, Color::from_hex("#123456"));
            assert_eq!(child.style.font_size, 22.0);
            assert_eq!(child.style.font_weight, 700);
        }

        #[test]
        fn controlled_input_dispatches_web_shaped_change_event() {
            let observed = Rc::new(RefCell::new(String::new()));
            let callback_observed = Rc::clone(&observed);
            let props = crate::aot_object();
            props.set_property(
                "onChange",
                Value::function(move |_, arguments| {
                    let value = arguments
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .get_property("target")
                        .get_property("value")
                        .to_js_string();
                    *callback_observed.borrow_mut() = value;
                    Value::Undefined
                }),
            );
            let component = render_to_component(createElement(vec![Value::from("input"), props]));
            let EventAction::NativeHost {
                id: host_id,
                input: true,
                ..
            } = component.on_click
            else {
                panic!("input did not register its native change callback");
            };

            dispatch_input_chain(&[host_id], "上海".to_string(), "海", "insertText", false);

            assert_eq!(observed.borrow().as_str(), "上海");
        }

        #[test]
        fn before_input_can_cancel_mutation_before_input_and_change() {
            let observed = Rc::new(RefCell::new(Vec::new()));
            let props = crate::aot_object();
            for (name, label, prevent) in [
                ("onBeforeInput", "beforeinput", true),
                ("onInput", "input", false),
                ("onChange", "change", false),
            ] {
                let observed = Rc::clone(&observed);
                props.set_property(
                    name,
                    Value::function(move |_, arguments| {
                        let event = &arguments[0];
                        observed.borrow_mut().push(format!(
                            "{}:{}:{}",
                            label,
                            event.get_property("inputType").to_js_string(),
                            event.get_property("data").to_js_string()
                        ));
                        if prevent {
                            event.call_method("preventDefault", Vec::new());
                        }
                        Value::Undefined
                    }),
                );
            }
            let component = render_to_component(createElement(vec![Value::from("input"), props]));
            let EventAction::NativeHost { id, .. } = component.on_click else {
                panic!("input did not retain native host identity");
            };

            assert!(dispatch_before_input_chain(
                &[id],
                "中",
                "insertCompositionText",
                true,
            ));
            assert_eq!(
                observed.borrow().as_slice(),
                &["beforeinput:insertCompositionText:中"]
            );

            dispatch_input_chain(&[id], "中".to_string(), "中", "insertCompositionText", true);
            assert_eq!(
                observed.borrow().as_slice(),
                &[
                    "beforeinput:insertCompositionText:中",
                    "input:insertCompositionText:中",
                    "change:insertCompositionText:中",
                ]
            );
        }

        #[test]
        fn composition_events_expose_phase_data_and_composing_state() {
            let observed = Rc::new(RefCell::new(Vec::new()));
            let props = crate::aot_object();
            for (name, phase) in [
                ("onCompositionStart", "start"),
                ("onCompositionUpdate", "update"),
                ("onCompositionEnd", "end"),
            ] {
                let observed = Rc::clone(&observed);
                props.set_property(
                    name,
                    Value::function(move |_, arguments| {
                        let event = &arguments[0];
                        observed.borrow_mut().push(format!(
                            "{}:{}:{}",
                            phase,
                            event.get_property("data").to_js_string(),
                            event.get_property("isComposing").to_js_string()
                        ));
                        Value::Undefined
                    }),
                );
            }
            let component = render_to_component(createElement(vec![Value::from("input"), props]));
            let EventAction::NativeHost { id, .. } = component.on_click else {
                panic!("input did not retain native host identity");
            };

            dispatch_composition_chain(&[id], "start", "");
            dispatch_composition_chain(&[id], "update", "zhong");
            dispatch_composition_chain(&[id], "end", "中");

            assert_eq!(
                observed.borrow().as_slice(),
                &["start::true", "update:zhong:true", "end:中:false"]
            );
        }

        #[test]
        fn automatic_jsx_runtime_key_does_not_replace_props_children() {
            let props = crate::aot_object();
            props.set_property("children", Value::from("content"));
            let element = call_host(
                "react/jsx-runtime::jsx",
                vec![Value::from("span"), props, Value::from("stable-key")],
            );

            let component = render_to_component(element);

            assert!(matches!(
                component.kind,
                ComponentKind::Text { ref content } if content == "content"
            ));
        }

        #[test]
        fn keyed_hosts_keep_identity_when_siblings_reorder() {
            let button = |key: &str, label: &str| {
                let props = crate::aot_object();
                props.set_property("children", Value::from(label));
                props.set_property("onClick", Value::function(|_, _| Value::Undefined));
                call_host(
                    "react/jsx-runtime::jsx",
                    vec![Value::from("button"), props, Value::from(key)],
                )
            };
            let first = render_to_component(Value::array(vec![button("a", "A"), button("b", "B")]));
            let second =
                render_to_component(Value::array(vec![button("b", "B"), button("a", "A")]));
            let host_id = |component: &Component| match component.on_click {
                EventAction::NativeHost {
                    id, click: true, ..
                } => id,
                _ => panic!("button did not keep native host identity"),
            };

            assert_eq!(host_id(&first.children[0]), host_id(&second.children[1]));
            assert_eq!(host_id(&first.children[1]), host_id(&second.children[0]));
        }

        #[test]
        fn unkeyed_host_inside_keyed_component_keeps_owner_identity() {
            let row = Value::function(|_, arguments| {
                let props = arguments.first().cloned().unwrap_or(Value::Undefined);
                let host_props = crate::aot_object();
                host_props.set_property("children", props.get_property("label"));
                createElement(vec![Value::from("div"), host_props])
            });
            let keyed_row = |key: &str, label: &str| {
                let props = crate::aot_object();
                props.set_property("label", Value::from(label));
                call_host(
                    "react/jsx-runtime::jsx",
                    vec![row.clone(), props, Value::from(key)],
                )
            };
            let first =
                render_to_component(Value::array(vec![keyed_row("a", "A"), keyed_row("b", "B")]));
            let second =
                render_to_component(Value::array(vec![keyed_row("b", "B"), keyed_row("a", "A")]));
            let host_id = |component: &Component| match component.on_click {
                EventAction::NativeHost { id, .. } => id,
                _ => panic!("component did not render a native Host"),
            };

            assert_eq!(host_id(&first.children[0]), host_id(&second.children[1]));
            assert_eq!(host_id(&first.children[1]), host_id(&second.children[0]));
            assert_ne!(host_id(&first.children[0]), host_id(&first.children[1]));
        }

        #[test]
        fn unmounted_keyed_subtrees_are_pruned_from_host_and_hook_caches() {
            let row = Value::function(|_, arguments| {
                let props = arguments.first().cloned().unwrap_or(Value::Undefined);
                let _state = useState(Value::Number(0.0));
                createElement(vec![
                    Value::from("div"),
                    crate::aot_object(),
                    props.get_property("label"),
                ])
            });
            let keyed_row = |key: &str| {
                let props = crate::aot_object();
                props.set_property("label", Value::from(key));
                call_host(
                    "react/jsx-runtime::jsx",
                    vec![row.clone(), props, Value::from(key)],
                )
            };

            for index in 0..100 {
                render_to_component(keyed_row(&format!("row-{index}")));
                assert_eq!(HOST_ELEMENTS.with(|elements| elements.borrow().len()), 1);
                assert_eq!(super::super::mounted_count(), 1);
            }

            render_to_component(Value::Null);
            assert_eq!(HOST_ELEMENTS.with(|elements| elements.borrow().len()), 0);
            assert_eq!(super::super::mounted_count(), 0);
        }

        #[test]
        fn click_chain_bubbles_and_honors_stop_propagation() {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let parent_calls = Rc::clone(&calls);
            let parent_props = crate::aot_object();
            parent_props.set_property(
                "onClick",
                Value::function(move |_, _| {
                    parent_calls.borrow_mut().push("parent");
                    Value::Undefined
                }),
            );
            let child_calls = Rc::clone(&calls);
            let child_props = crate::aot_object();
            child_props.set_property(
                "onClick",
                Value::function(move |_, arguments| {
                    child_calls.borrow_mut().push("child");
                    arguments[0].call_method("stopPropagation", Vec::new());
                    Value::Undefined
                }),
            );
            let child = createElement(vec![
                Value::from("button"),
                child_props,
                Value::from("child"),
            ]);
            let tree =
                render_to_component(createElement(vec![Value::from("div"), parent_props, child]));
            let EventAction::NativeHost {
                id: parent_id,
                click: true,
                ..
            } = tree.on_click
            else {
                panic!("parent click was not registered");
            };
            let EventAction::NativeHost {
                id: child_id,
                click: true,
                ..
            } = tree.children[0].on_click
            else {
                panic!("child click was not registered");
            };

            dispatch_click_chain(&[child_id, parent_id]);

            assert_eq!(calls.borrow().as_slice(), &["child"]);
        }

        #[test]
        fn bubbling_keeps_the_actual_child_as_event_target() {
            let observed = Rc::new(RefCell::new(String::new()));
            let callback_observed = Rc::clone(&observed);
            let parent_props = crate::aot_object();
            parent_props.set_property(
                "onClick",
                Value::function(move |_, arguments| {
                    *callback_observed.borrow_mut() = arguments[0]
                        .get_property("target")
                        .get_property("localName")
                        .to_js_string();
                    Value::Undefined
                }),
            );
            let child = createElement(vec![
                Value::from("span"),
                crate::aot_object(),
                Value::from("child"),
            ]);
            let tree =
                render_to_component(createElement(vec![Value::from("div"), parent_props, child]));
            let host_id = |component: &Component| match component.on_click {
                EventAction::NativeHost { id, .. } => id,
                _ => panic!("intrinsic did not retain native host identity"),
            };

            dispatch_click_chain(&[host_id(&tree.children[0]), host_id(&tree)]);

            assert_eq!(observed.borrow().as_str(), "span");
        }

        #[test]
        fn keyboard_event_exposes_web_fields_and_prevent_default() {
            let observed = Rc::new(RefCell::new(Vec::new()));
            let callback_observed = Rc::clone(&observed);
            let props = crate::aot_object();
            props.set_property(
                "onKeyDown",
                Value::function(move |_, arguments| {
                    let event = &arguments[0];
                    callback_observed.borrow_mut().extend([
                        event.get_property("key").to_js_string(),
                        event.get_property("code").to_js_string(),
                        event.get_property("shiftKey").to_js_string(),
                    ]);
                    event.call_method("preventDefault", Vec::new());
                    Value::Undefined
                }),
            );
            let component = render_to_component(createElement(vec![Value::from("input"), props]));
            let EventAction::NativeHost { id, .. } = component.on_click else {
                panic!("input did not retain native host identity");
            };

            let prevented = dispatch_key_chain(
                &[id],
                "Enter",
                "Enter",
                false,
                false,
                false,
                false,
                true,
                true,
            );

            assert!(prevented);
            assert_eq!(observed.borrow().as_slice(), &["Enter", "Enter", "true"]);
        }

        #[test]
        fn focus_and_blur_use_the_same_stable_host_target() {
            let observed = Rc::new(RefCell::new(Vec::new()));
            let focus_observed = Rc::clone(&observed);
            let blur_observed = Rc::clone(&observed);
            let props = crate::aot_object();
            props.set_property(
                "onFocus",
                Value::function(move |_, arguments| {
                    focus_observed.borrow_mut().push(
                        arguments[0]
                            .get_property("target")
                            .get_property("localName")
                            .to_js_string(),
                    );
                    Value::Undefined
                }),
            );
            props.set_property(
                "onBlur",
                Value::function(move |_, arguments| {
                    blur_observed
                        .borrow_mut()
                        .push(arguments[0].get_property("type").to_js_string());
                    Value::Undefined
                }),
            );
            let component = render_to_component(createElement(vec![Value::from("input"), props]));
            let EventAction::NativeHost { id, .. } = component.on_click else {
                panic!("input did not retain native host identity");
            };

            dispatch_focus_chain(&[id], true);
            dispatch_focus_chain(&[id], false);

            assert_eq!(observed.borrow().as_slice(), &["input", "blur"]);
        }

        #[test]
        fn enter_submit_targets_form_and_is_cancelable() {
            let observed = Rc::new(RefCell::new(String::new()));
            let callback_observed = Rc::clone(&observed);
            let form_props = crate::aot_object();
            form_props.set_property(
                "onSubmit",
                Value::function(move |_, arguments| {
                    let event = &arguments[0];
                    *callback_observed.borrow_mut() = event
                        .get_property("target")
                        .get_property("localName")
                        .to_js_string();
                    event.call_method("preventDefault", Vec::new());
                    Value::Undefined
                }),
            );
            let input = createElement(vec![Value::from("input"), crate::aot_object()]);
            let tree =
                render_to_component(createElement(vec![Value::from("form"), form_props, input]));
            let host_id = |component: &Component| match component.on_click {
                EventAction::NativeHost { id, .. } => id,
                _ => panic!("intrinsic did not retain native host identity"),
            };

            let prevented = dispatch_submit_chain(&[host_id(&tree.children[0]), host_id(&tree)])
                .expect("form submit listener was not dispatched");

            assert!(prevented);
            assert_eq!(observed.borrow().as_str(), "form");
        }

        #[test]
        fn mouse_pointer_dispatches_pointer_then_compat_mouse_with_coordinates() {
            let observed = Rc::new(RefCell::new(Vec::new()));
            let pointer_observed = Rc::clone(&observed);
            let mouse_observed = Rc::clone(&observed);
            let props = crate::aot_object();
            props.set_property(
                "onPointerDown",
                Value::function(move |_, arguments| {
                    let event = &arguments[0];
                    pointer_observed.borrow_mut().push(format!(
                        "{}:{}:{}:{}:{}",
                        event.get_property("type").to_js_string(),
                        event.get_property("clientX").to_js_string(),
                        event.get_property("pointerType").to_js_string(),
                        event.get_property("ctrlKey").to_js_string(),
                        event.get_property("shiftKey").to_js_string()
                    ));
                    Value::Undefined
                }),
            );
            props.set_property(
                "onMouseDown",
                Value::function(move |_, arguments| {
                    mouse_observed
                        .borrow_mut()
                        .push(arguments[0].get_property("type").to_js_string());
                    Value::Undefined
                }),
            );
            let component = render_to_component(createElement(vec![Value::from("button"), props]));
            let EventAction::NativeHost { id, .. } = component.on_click else {
                panic!("button did not retain native host identity");
            };

            let prevented = dispatch_pointer_chain(
                &[id],
                "down",
                12.0,
                34.0,
                1,
                "mouse",
                0,
                1,
                0.5,
                true,
                false,
                true,
                false,
                true,
            );

            assert!(!prevented);
            assert_eq!(
                observed.borrow().as_slice(),
                &["pointerdown:12:mouse:true:true", "mousedown"]
            );
        }

        #[test]
        fn wheel_event_prevent_default_is_reported_to_runtime() {
            let props = crate::aot_object();
            props.set_property(
                "onWheel",
                Value::function(|_, arguments| {
                    let event = &arguments[0];
                    assert_eq!(event.get_property("deltaY").to_number(), 48.0);
                    assert_eq!(event.get_property("deltaMode").to_number(), 1.0);
                    event.call_method("preventDefault", Vec::new());
                    Value::Undefined
                }),
            );
            let component = render_to_component(createElement(vec![Value::from("div"), props]));
            let EventAction::NativeHost { id, .. } = component.on_click else {
                panic!("wheel host did not retain native host identity");
            };

            assert!(dispatch_wheel_chain(
                &[id],
                8.0,
                16.0,
                0.0,
                48.0,
                1,
                false,
                false,
                false,
                false,
            ));
        }
    }
}

fn aot_object() -> w3cos_core::Value {
    w3cos_core::Value::object(std::collections::HashMap::new())
}

// ---------------------------------------------------------------------------
// Internal slot model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComponentId(pub u64);

enum HookSlot {
    State(Box<dyn Any>),
    Effect {
        deps: Vec<u64>,
        cleanup: Option<Box<dyn FnOnce()>>,
    },
    Memo {
        deps: Vec<u64>,
        value: Box<dyn Any>,
    },
    Ref(Rc<RefCell<Box<dyn Any>>>),
    Reducer(Box<dyn Any>),
    Context,
}

#[derive(Default)]
struct ComponentInstance {
    slots: Vec<HookSlot>,
    /// Effects whose cleanup should run when the component is dropped.
    pending_cleanups: Vec<Box<dyn FnOnce()>>,
}

#[derive(Default)]
struct HostState {
    components: HashMap<ComponentId, ComponentInstance>,
    /// Components whose dependencies changed and that should be re-rendered
    /// on the next host tick.
    dirty: Vec<ComponentId>,
    /// Stack of currently-rendering components (for nested rendering).
    active: Vec<RenderFrame>,
    /// Globally provided context values keyed by `(component_root, type-name)`.
    contexts: HashMap<&'static str, Box<dyn Any>>,
}

struct RenderFrame {
    id: ComponentId,
    cursor: usize,
}

thread_local! {
    static HOST: RefCell<HostState> = RefCell::new(HostState::default());
}

fn with_host<R>(f: impl FnOnce(&mut HostState) -> R) -> R {
    HOST.with(|h| f(&mut h.borrow_mut()))
}

// ---------------------------------------------------------------------------
// Render lifecycle
// ---------------------------------------------------------------------------

/// Allocate (or reuse) state for `id` and start a render frame. Hook calls
/// inside this scope read from `id`'s slot table in declaration order.
///
/// Must be paired with [`end_render`].
pub fn begin_render(id: ComponentId) {
    with_host(|host| {
        host.components.entry(id).or_default();
        host.active.push(RenderFrame { id, cursor: 0 });
    });
}

/// Close the current render frame. Asserts that `id` matches the
/// most-recent [`begin_render`].
pub fn end_render(id: ComponentId) {
    with_host(|host| {
        let frame = host.active.pop().expect("end_render without begin_render");
        debug_assert_eq!(frame.id, id, "render frame id mismatch");
    });
}

/// Check whether the component is currently mid-render. Useful as a guard
/// for hook calls outside the render lifecycle.
pub fn is_rendering() -> bool {
    with_host(|h| !h.active.is_empty())
}

/// Mark `id` for re-render. Hosts poll [`take_dirty`] each tick.
pub fn mark_dirty(id: ComponentId) {
    with_host(|host| {
        if !host.dirty.contains(&id) {
            host.dirty.push(id);
        }
    });
}

/// Whether React has queued a component render, without consuming the queue.
pub fn has_dirty() -> bool {
    with_host(|host| !host.dirty.is_empty())
}

/// Drain components flagged for re-render. Intended for the runtime's
/// reactive loop.
pub fn take_dirty() -> Vec<ComponentId> {
    with_host(|host| std::mem::take(&mut host.dirty))
}

/// Unmount a component — runs all pending effect cleanups and frees state.
pub fn unmount(id: ComponentId) {
    let cleanups = with_host(|host| {
        host.dirty.retain(|d| *d != id);
        host.components
            .remove(&id)
            .map(|c| c.pending_cleanups)
            .unwrap_or_default()
    });
    for cleanup in cleanups {
        cleanup();
    }
}

/// Total number of mounted components — exposed for diagnostics.
pub fn mounted_count() -> usize {
    with_host(|h| h.components.len())
}

// ---------------------------------------------------------------------------
// Hook helpers
// ---------------------------------------------------------------------------

fn current_frame_cursor() -> (ComponentId, usize) {
    with_host(|host| {
        let frame = host
            .active
            .last_mut()
            .expect("hook called outside of begin_render/end_render scope");
        let cursor = frame.cursor;
        frame.cursor += 1;
        (frame.id, cursor)
    })
}

fn ensure_slot<F: FnOnce() -> HookSlot>(id: ComponentId, idx: usize, init: F) {
    with_host(|host| {
        let comp = host.components.get_mut(&id).expect("component missing");
        if comp.slots.len() <= idx {
            comp.slots.push(init());
        }
    });
}

fn with_slot<R, F>(id: ComponentId, idx: usize, f: F) -> R
where
    F: FnOnce(&mut HookSlot) -> R,
{
    with_host(|host| {
        let comp = host.components.get_mut(&id).expect("component missing");
        let slot = comp.slots.get_mut(idx).expect("hook slot missing");
        f(slot)
    })
}

// ---------------------------------------------------------------------------
// useState
// ---------------------------------------------------------------------------

/// `useState` — returns a [`StateHook`] wrapping a [`Signal`].
pub fn use_state<T>(initial: impl FnOnce() -> T) -> StateHook<T>
where
    T: Clone + PartialEq + 'static,
{
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || {
        HookSlot::State(Box::new(Signal::new(initial())))
    });
    let signal = with_slot(id, idx, |slot| {
        if let HookSlot::State(boxed) = slot {
            boxed
                .downcast_ref::<Signal<T>>()
                .expect("useState slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useState");
        }
    });

    // Subscribe the host re-render to value changes for this component.
    let dirty_id = id;
    signal.subscribe(Rc::new(move || mark_dirty(dirty_id)));

    StateHook { signal }
}

/// Handle returned from [`use_state`]. Mirrors `[value, setValue]` from React.
pub struct StateHook<T: Clone + PartialEq + 'static> {
    signal: Signal<T>,
}

impl<T: Clone + PartialEq + 'static> StateHook<T> {
    pub fn get(&self) -> T {
        self.signal.get_untracked()
    }

    pub fn set(&self, value: T) {
        self.signal.set(value);
    }

    pub fn update(&self, f: impl FnOnce(&T) -> T) {
        self.signal.update(f);
    }

    /// Stable identity used by [`use_effect`] / [`use_memo`] / [`use_callback`]
    /// when listing this state as a dependency.
    pub fn deps(&self) -> u64 {
        self.signal.id() as u64
    }

    pub fn signal(&self) -> Signal<T> {
        self.signal.clone()
    }
}

impl<T: Clone + PartialEq + 'static> Clone for StateHook<T> {
    fn clone(&self) -> Self {
        Self {
            signal: self.signal.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// useEffect
// ---------------------------------------------------------------------------

/// `useEffect(fn, deps)` — runs `fn` after each render whose dependency list
/// changed. Returning a cleanup closure mirrors React's cleanup semantics.
pub fn use_effect<F>(effect: F, deps: &[u64])
where
    F: FnOnce() -> Option<Box<dyn FnOnce()>> + 'static,
{
    let (id, idx) = current_frame_cursor();
    let mut should_run = false;

    ensure_slot(id, idx, || {
        should_run = true;
        HookSlot::Effect {
            deps: deps.to_vec(),
            cleanup: None,
        }
    });

    if !should_run {
        let prev_deps = with_slot(id, idx, |slot| {
            if let HookSlot::Effect { deps: prev, .. } = slot {
                prev.clone()
            } else {
                panic!("hook slot at index {idx} was not useEffect");
            }
        });
        if prev_deps != deps {
            should_run = true;
        }
    }

    if should_run {
        // Run cleanup from prior invocation first.
        let prior_cleanup = with_slot(id, idx, |slot| {
            if let HookSlot::Effect {
                deps: prev,
                cleanup,
            } = slot
            {
                *prev = deps.to_vec();
                cleanup.take()
            } else {
                None
            }
        });
        if let Some(c) = prior_cleanup {
            c();
        }
        let cleanup = effect();
        with_slot(id, idx, |slot| {
            if let HookSlot::Effect {
                cleanup: stored, ..
            } = slot
            {
                *stored = cleanup;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// useMemo
// ---------------------------------------------------------------------------

/// `useMemo(compute, deps)` — caches the value produced by `compute` and
/// re-runs it whenever any dependency changes.
pub fn use_memo<T, F>(compute: F, deps: &[u64]) -> MemoHook<T>
where
    F: FnOnce() -> T,
    T: Clone + 'static,
{
    let (id, idx) = current_frame_cursor();

    let mut needs_compute = false;
    ensure_slot(id, idx, || {
        needs_compute = true;
        HookSlot::Memo {
            deps: deps.to_vec(),
            value: Box::new(()), // placeholder — replaced immediately below
        }
    });

    if !needs_compute {
        let prev_deps = with_slot(id, idx, |slot| {
            if let HookSlot::Memo { deps: prev, .. } = slot {
                prev.clone()
            } else {
                panic!("hook slot at index {idx} was not useMemo");
            }
        });
        if prev_deps != deps {
            needs_compute = true;
        }
    }

    if needs_compute {
        let new_value = compute();
        with_slot(id, idx, |slot| {
            if let HookSlot::Memo {
                deps: stored_deps,
                value,
            } = slot
            {
                *stored_deps = deps.to_vec();
                *value = Box::new(new_value);
            }
        });
    }

    let value = with_slot(id, idx, |slot| {
        if let HookSlot::Memo { value, .. } = slot {
            value
                .downcast_ref::<T>()
                .expect("useMemo slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useMemo");
        }
    });

    MemoHook { value }
}

pub struct MemoHook<T> {
    value: T,
}

impl<T: Clone> MemoHook<T> {
    pub fn get(&self) -> T {
        self.value.clone()
    }
}

// ---------------------------------------------------------------------------
// useCallback (memoised closure container)
// ---------------------------------------------------------------------------

/// `useCallback(fn, deps)` — memoises a closure. Identity is preserved
/// across renders unless `deps` changes. Internally backed by
/// `Rc<RefCell<...>>` so callers can call it multiple times.
pub fn use_callback<F>(callback: F, deps: &[u64]) -> Rc<RefCell<F>>
where
    F: 'static,
{
    let (id, idx) = current_frame_cursor();
    let mut needs_replace = false;
    ensure_slot(id, idx, || {
        needs_replace = true;
        HookSlot::Memo {
            deps: deps.to_vec(),
            value: Box::new(()),
        }
    });

    if !needs_replace {
        let prev_deps = with_slot(id, idx, |slot| {
            if let HookSlot::Memo { deps: prev, .. } = slot {
                prev.clone()
            } else {
                panic!("hook slot at index {idx} was not useCallback");
            }
        });
        if prev_deps != deps {
            needs_replace = true;
        }
    }

    if needs_replace {
        let cell = Rc::new(RefCell::new(callback));
        with_slot(id, idx, |slot| {
            if let HookSlot::Memo {
                deps: stored,
                value,
            } = slot
            {
                *stored = deps.to_vec();
                *value = Box::new(cell.clone());
            }
        });
    }

    with_slot(id, idx, |slot| {
        if let HookSlot::Memo { value, .. } = slot {
            value
                .downcast_ref::<Rc<RefCell<F>>>()
                .expect("useCallback slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useCallback");
        }
    })
}

// ---------------------------------------------------------------------------
// useRef
// ---------------------------------------------------------------------------

/// `useRef(initial)` — returns a stable mutable reference. Mutating the
/// ref does NOT trigger a re-render.
pub fn use_ref<T>(initial: impl FnOnce() -> T) -> RefHook<T>
where
    T: 'static,
{
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || {
        HookSlot::Ref(Rc::new(RefCell::new(Box::new(initial()))))
    });
    let inner = with_slot(id, idx, |slot| {
        if let HookSlot::Ref(rc) = slot {
            Rc::clone(rc)
        } else {
            panic!("hook slot at index {idx} was not useRef");
        }
    });
    RefHook {
        inner,
        _phantom: std::marker::PhantomData,
    }
}

pub struct RefHook<T: 'static> {
    inner: Rc<RefCell<Box<dyn Any>>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: 'static> RefHook<T> {
    /// `ref.current` (mutable).
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.borrow_mut();
        let current = guard.downcast_mut::<T>().expect("useRef type mismatch");
        f(current)
    }

    /// `ref.current` (read-only).
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard = self.inner.borrow();
        let current = guard.downcast_ref::<T>().expect("useRef type mismatch");
        f(current)
    }
}

impl<T: 'static> Clone for RefHook<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
            _phantom: std::marker::PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// useReducer
// ---------------------------------------------------------------------------

/// `useReducer(reducer, initial)` — Redux-style state container.
pub fn use_reducer<S, A, R>(reducer: R, initial: impl FnOnce() -> S) -> ReducerHook<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
    R: Fn(S, A) -> S + 'static,
{
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || {
        let state = Signal::new(initial());
        let pair = ReducerPair::<S, A> {
            state,
            reducer: Rc::new(reducer),
        };
        HookSlot::Reducer(Box::new(pair))
    });
    let pair = with_slot(id, idx, |slot| {
        if let HookSlot::Reducer(boxed) = slot {
            boxed
                .downcast_ref::<ReducerPair<S, A>>()
                .expect("useReducer slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useReducer");
        }
    });

    let dirty_id = id;
    pair.state.subscribe(Rc::new(move || mark_dirty(dirty_id)));

    ReducerHook { pair }
}

struct ReducerPair<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    state: Signal<S>,
    reducer: Rc<dyn Fn(S, A) -> S>,
}

impl<S, A> Clone for ReducerPair<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            reducer: Rc::clone(&self.reducer),
        }
    }
}

pub struct ReducerHook<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    pair: ReducerPair<S, A>,
}

impl<S, A> ReducerHook<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    pub fn get(&self) -> S {
        self.pair.state.get_untracked()
    }

    pub fn dispatch(&self, action: A) {
        let next = (self.pair.reducer)(self.pair.state.get_untracked(), action);
        self.pair.state.set(next);
    }

    pub fn deps(&self) -> u64 {
        self.pair.state.id() as u64
    }
}

// ---------------------------------------------------------------------------
// useContext
// ---------------------------------------------------------------------------

/// `provide_context::<T>(value)` — sets a context value visible to subsequent
/// [`use_context`] calls. Mirrors `<Context.Provider>`.
pub fn provide_context<T: Clone + 'static>(value: T) {
    let key = std::any::type_name::<T>();
    with_host(|host| {
        host.contexts.insert(key, Box::new(value));
    });
}

/// `useContext::<T>()` — returns the most-recently-provided value of type
/// `T`, or the supplied default.
pub fn use_context<T: Clone + 'static>(default: impl FnOnce() -> T) -> T {
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || HookSlot::Context);
    let _slot_taken = with_slot(id, idx, |_| ());

    let key = std::any::type_name::<T>();
    let from_provider = with_host(|host| {
        host.contexts
            .get(key)
            .and_then(|boxed| boxed.downcast_ref::<T>().cloned())
    });
    from_provider.unwrap_or_else(default)
}

// ---------------------------------------------------------------------------
// Utility re-exports
// ---------------------------------------------------------------------------

pub use w3cos_core::{Effect as ReactiveEffect, Signal as ReactiveSignal};

/// `flushSync(fn)` — run `fn` inside a reactive batch so that multiple
/// state updates collapse into a single re-render notification.
pub fn flush_sync(f: impl FnOnce()) {
    batch(f);
}

/// Convenience: pretend to be a top-level component for ad-hoc tests / demos.
pub fn render<F: FnOnce()>(id: u64, f: F) {
    let cid = ComponentId(id);
    begin_render(cid);
    f();
    end_render(cid);
}

/// Reset all hook state (tests).
pub fn reset_all() {
    let cleanups = with_host(|host| {
        host.dirty.clear();
        host.active.clear();
        host.contexts.clear();
        host.components
            .drain()
            .flat_map(|(_, c)| c.pending_cleanups.into_iter())
            .collect::<Vec<_>>()
    });
    for cleanup in cleanups {
        cleanup();
    }
}

#[allow(unused)]
fn _capture_unused_imports() {
    // Silences unused-import warnings while keeping the explicit `Effect`
    // re-export documented above.
    let _ = std::mem::size_of::<Effect>();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_GUARD.lock().unwrap();
        reset_all();
        guard
    }

    #[test]
    fn use_state_persists_across_renders() {
        let _g = fresh();
        let id = ComponentId(1);

        begin_render(id);
        let s = use_state::<i32>(|| 0);
        s.set(42);
        end_render(id);

        begin_render(id);
        let s2 = use_state::<i32>(|| 999);
        let val: i32 = s2.get();
        assert_eq!(val, 42, "useState must keep its value across renders");
        end_render(id);
    }

    #[test]
    fn setting_state_marks_dirty() {
        let _g = fresh();
        let id = ComponentId(2);
        begin_render(id);
        let s = use_state::<i32>(|| 0);
        end_render(id);
        let _ = take_dirty();

        s.set(10);
        let dirty = take_dirty();
        assert!(dirty.contains(&id));
    }

    #[test]
    fn checking_dirty_does_not_consume_pending_render() {
        let _g = fresh();
        let id = ComponentId(22);
        begin_render(id);
        let state = use_state::<i32>(|| 0);
        end_render(id);
        let _ = take_dirty();

        state.set(1);
        assert!(has_dirty());
        assert!(has_dirty(), "polling dirty state must be non-destructive");
        assert_eq!(take_dirty(), vec![id]);
    }

    #[test]
    fn use_effect_runs_on_dep_change() {
        let _g = fresh();
        let id = ComponentId(3);
        let runs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

        let logs1 = runs.clone();
        begin_render(id);
        let s = use_state::<i32>(|| 1);
        let v: i32 = s.get();
        use_effect(
            move || {
                logs1.borrow_mut().push("first".into());
                None
            },
            &[s.deps(), v as u64],
        );
        end_render(id);

        let logs2 = runs.clone();
        begin_render(id);
        let s = use_state::<i32>(|| 1);
        let v: i32 = s.get();
        use_effect(
            move || {
                logs2.borrow_mut().push("second".into());
                None
            },
            &[s.deps(), v as u64],
        );
        end_render(id);

        // Same deps → effect should NOT re-run.
        assert_eq!(runs.borrow().len(), 1);

        // Change a dep value: now we expect the effect to fire again.
        s.set(2);

        let logs3 = runs.clone();
        begin_render(id);
        let s = use_state::<i32>(|| 1);
        let v: i32 = s.get();
        use_effect(
            move || {
                logs3.borrow_mut().push("third".into());
                None
            },
            &[s.deps(), v as u64],
        );
        end_render(id);
        assert_eq!(runs.borrow().len(), 2);
    }

    #[test]
    fn use_memo_skips_when_deps_unchanged() {
        let _g = fresh();
        let id = ComponentId(4);
        let calls = Rc::new(RefCell::new(0usize));

        for _ in 0..3 {
            let calls = calls.clone();
            begin_render(id);
            let _ = use_state::<i32>(|| 0);
            let m = use_memo(
                move || {
                    *calls.borrow_mut() += 1;
                    7 * 6
                },
                &[1, 2, 3],
            );
            assert_eq!(m.get(), 42);
            end_render(id);
        }
        assert_eq!(*calls.borrow(), 1, "memo recomputed despite stable deps");
    }

    #[test]
    fn use_ref_is_stable() {
        let _g = fresh();
        let id = ComponentId(5);

        begin_render(id);
        let r = use_ref::<Vec<i32>>(Vec::new);
        r.with_mut(|v| v.push(7));
        end_render(id);

        begin_render(id);
        let r2 = use_ref::<Vec<i32>>(Vec::new);
        let len = r2.with(|v| v.len());
        assert_eq!(len, 1);
        end_render(id);
    }

    #[test]
    fn reducer_dispatch() {
        let _g = fresh();
        let id = ComponentId(6);
        begin_render(id);
        let r = use_reducer::<i32, i32, _>(|state, action| state + action, || 10);
        r.dispatch(5);
        r.dispatch(2);
        assert_eq!(r.get(), 17);
        end_render(id);
    }

    #[test]
    fn context_propagation() {
        let _g = fresh();
        provide_context::<&'static str>("dark-theme");

        let id = ComponentId(7);
        begin_render(id);
        let theme = use_context::<&'static str>(|| "default");
        assert_eq!(theme, "dark-theme");
        end_render(id);
    }

    #[test]
    fn unmount_cleans_state() {
        let _g = fresh();
        let id = ComponentId(8);
        begin_render(id);
        let _s = use_state::<u8>(|| 1);
        end_render(id);
        assert_eq!(mounted_count(), 1);
        unmount(id);
        assert_eq!(mounted_count(), 0);
    }

    #[test]
    fn flush_sync_batches_updates() {
        let _g = fresh();
        let id = ComponentId(9);
        begin_render(id);
        let s = use_state::<i32>(|| 0);
        end_render(id);

        flush_sync(|| {
            s.set(1);
            s.set(2);
            s.set(3);
        });
        let final_val: i32 = s.get();
        assert_eq!(final_val, 3);
    }
}
