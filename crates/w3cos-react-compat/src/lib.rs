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
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use w3cos_core::Value;
    use w3cos_std::color::Color;
    use w3cos_std::component::Component;
    use w3cos_std::style::{
        AlignItems, Dimension, Edges, FlexDirection, JustifyContent, Overflow, Position, Style,
        Transform2D,
    };

    thread_local! {
        static NEXT_AOT_COMPONENT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
        static NEXT_HOST_ELEMENT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
        static HOST_ELEMENTS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static SCROLL_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }

    fn deps(value: &Value) -> Vec<u64> {
        let Value::Array(values) = value else {
            return Vec::new();
        };
        values
            .borrow()
            .iter()
            .map(|value| {
                let mut hasher = DefaultHasher::new();
                value.to_js_string().hash(&mut hasher);
                hasher.finish()
            })
            .collect()
    }

    pub fn call_host(path: &str, arguments: Vec<Value>) -> Value {
        let argument = |index| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match path.rsplit("::").next().unwrap_or(path) {
            "useState" => useState(argument(0)),
            "useMemo" => useMemo(argument(0), argument(1)),
            "useCallback" => useCallback(argument(0), argument(1)),
            "useRef" => useRef(argument(0)),
            "useEffect" => useEffect(argument(0), argument(1)),
            "useLayoutEffect" => useLayoutEffect(argument(0), argument(1)),
            "useImperativeHandle" => useImperativeHandle(argument(0), argument(1), argument(2)),
            "memo" => memo(argument(0)),
            "createElement" => createElement(arguments),
            "jsx" | "jsxs" => createElement(arguments),
            "Fragment" => Fragment(),
            _ => Value::Undefined,
        }
    }

    pub fn render_to_component(value: Value) -> Component {
        NEXT_AOT_COMPONENT.with(|next| next.set(1));
        NEXT_HOST_ELEMENT.with(|next| next.set(1));
        render_value(value)
    }

    pub fn dispatch_scroll(host_id: u64, offset: f32) {
        HOST_ELEMENTS.with(|elements| {
            if let Some(element) = elements.borrow().get(&host_id) {
                element.set_property("scrollTop", Value::Number(offset as f64));
            }
        });
        let listener = SCROLL_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned());
        if let Some(listener) = listener {
            listener.call(
                Value::Undefined,
                vec![Value::object(std::collections::HashMap::new())],
            );
        }
    }

    pub fn has_pending_render() -> bool {
        !super::take_dirty().is_empty()
    }

    pub fn component_count(component: &Component) -> usize {
        1 + component
            .children
            .iter()
            .map(component_count)
            .sum::<usize>()
    }

    fn render_value(value: Value) -> Component {
        match value {
            Value::Array(values) => Component::column(
                Style::default(),
                values.borrow().iter().cloned().map(render_value).collect(),
            ),
            Value::String(text) => Component::text(text, Style::default()),
            Value::Number(number) => {
                Component::text(Value::Number(number).to_js_string(), Style::default())
            }
            Value::Object(_) => {
                let element_type = value.get_property("type");
                if element_type.is_undefined() {
                    return Component::boxed(Style::default(), Vec::new());
                }
                let props = value.get_property("props");
                if element_type.is_function() {
                    let id = NEXT_AOT_COMPONENT.with(|next| {
                        let id = next.get();
                        next.set(id + 1);
                        super::ComponentId(id)
                    });
                    super::begin_render(id);
                    let rendered = element_type.call(Value::Undefined, vec![props]);
                    super::end_render(id);
                    if std::env::var_os("W3COS_AOT_TRACE").is_some() {
                        eprintln!("[w3cos-aot] component {id:?} -> {}", value_shape(&rendered));
                    }
                    return render_value(rendered);
                }
                let host_id = NEXT_HOST_ELEMENT.with(|next| {
                    let id = next.get();
                    next.set(id + 1);
                    id
                });
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
                if created {
                    let reference = props.get_property("ref_");
                    if reference.is_function() {
                        reference.call(Value::Undefined, vec![host]);
                    }
                }
                let children = props.get_property("children");
                let children = match children {
                    Value::Array(values) => {
                        values.borrow().iter().cloned().map(render_value).collect()
                    }
                    Value::Undefined | Value::Null => Vec::new(),
                    child => vec![render_value(child)],
                };
                let mut component = match element_type.to_js_string().as_str() {
                    "span" | "p" => {
                        let text = children
                            .iter()
                            .filter_map(|child| match &child.kind {
                                w3cos_std::component::ComponentKind::Text { content } => {
                                    Some(content.as_str())
                                }
                                _ => None,
                            })
                            .collect::<String>();
                        Component::text(text, style_from_props(&props))
                    }
                    "button" => Component::button("", Style::default()),
                    _ => Component::boxed(style_from_props(&props), children),
                };
                if SCROLL_LISTENERS.with(|listeners| listeners.borrow().contains_key(&host_id)) {
                    component.on_click = w3cos_std::EventAction::NativeScroll(host_id);
                }
                component
            }
            Value::Function(function) => render_value(function.call(Value::Undefined, vec![])),
            Value::Bool(boolean) => Component::text(boolean.to_string(), Style::default()),
            Value::Undefined | Value::Null => Component::boxed(Style::default(), Vec::new()),
        }
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
        host.set_property("scrollTop", Value::Number(0.0));
        host.set_property("scrollLeft", Value::Number(0.0));
        host.set_property("children", Value::array(Vec::new()));
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

    fn style_from_props(props: &Value) -> Style {
        let source = props.get_property("style");
        let mut style = Style::default();
        style.width = dimension(&source.get_property("width"));
        style.height = dimension(&source.get_property("height"));
        style.max_width = dimension(&source.get_property("maxWidth"));
        style.max_height = dimension(&source.get_property("maxHeight"));
        style.flex_grow = source.get_property("flexGrow").to_number().max(0.0) as f32;
        let flex_shrink = source.get_property("flexShrink").to_number();
        if flex_shrink.is_finite() {
            style.flex_shrink = flex_shrink.max(0.0) as f32;
        }
        style.flex_direction = match source.get_property("flexDirection").to_js_string().as_str() {
            "row" => FlexDirection::Row,
            "row-reverse" => FlexDirection::RowReverse,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => FlexDirection::Column,
        };
        style.justify_content = match source.get_property("justifyContent").to_js_string().as_str() {
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
        let padding = source.get_property("padding").to_number();
        if padding.is_finite() {
            style.padding = Edges::all(padding as f32);
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
        style.position = match source.get_property("position").to_js_string().as_str() {
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            "sticky" => Position::Sticky,
            _ => Position::Relative,
        };
        style.overflow = match source.get_property("overflowY").to_js_string().as_str() {
            "auto" => Overflow::Auto,
            "scroll" => Overflow::Scroll,
            "hidden" => Overflow::Hidden,
            _ => Overflow::Visible,
        };
        let transform = source.get_property("transform").to_js_string();
        if let Some(value) = transform
            .strip_prefix("translateY(")
            .and_then(|value| value.strip_suffix("px)"))
        {
            let translate_y = value.parse().unwrap_or(0.0);
            if matches!(style.position, Position::Absolute) {
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
        }
        style
    }

    fn css_color(value: &Value) -> Option<Color> {
        let value = value.to_js_string();
        (value.starts_with('#') || value == "transparent").then(|| Color::from_hex(&value))
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
            _ => Dimension::Auto,
        }
    }

    pub fn useState(initial: Value) -> Value {
        let initial = if initial.is_function() {
            initial.call(Value::Undefined, vec![])
        } else {
            initial
        };
        if std::env::var_os("W3COS_AOT_TRACE").is_some() {
            eprintln!(
                "[w3cos-aot] useState {} range=({}, {})",
                value_shape(&initial),
                initial.get_property("startIndexOverscan").to_js_string(),
                initial.get_property("stopIndexOverscan").to_js_string()
            );
        }
        let state = use_state(|| initial);
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
        Value::array(vec![state.get(), setter])
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
                let cleanup = effect.call(Value::Undefined, vec![]);
                cleanup.is_function().then(|| {
                    Box::new(move || {
                        cleanup.call(Value::Undefined, vec![]);
                    }) as Box<dyn FnOnce()>
                })
            },
            &dependencies,
        );
        Value::Undefined
    }

    pub fn useLayoutEffect(effect: Value, dependencies: Value) -> Value {
        useEffect(effect, dependencies)
    }

    pub fn memo(component: Value) -> Value {
        component
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

    pub fn useImperativeHandle(reference: Value, factory: Value, dependencies: Value) -> Value {
        let value = useMemo(factory, dependencies);
        reference.set_property("current", value);
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

        #[test]
        fn absolute_translate_y_positions_the_entire_host_subtree() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("position", Value::from("absolute"));
            style.set_property("transform", Value::from("translateY(76px)"));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style);

            let native = style_from_props(&props);
            assert!(matches!(native.position, Position::Absolute));
            assert!(matches!(native.top, Dimension::Px(value) if value == 76.0));
            assert!(native.transform.is_identity());
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
