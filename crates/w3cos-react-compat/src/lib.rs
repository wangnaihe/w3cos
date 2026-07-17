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
        AlignContent, AlignItems, AlignSelf, BoxShadow, Cursor, Dimension, Display, Easing, Edges,
        FlexDirection, FlexWrap, FontStyle, JustifyContent, OutlineStyle, Overflow, PointerEvents,
        Position, Spacing, Style, TextAlign, TextOverflow, Transform2D, Transition,
        TransitionProperty, UserSelect, Visibility, WhiteSpace, WillChange, WordBreak,
    };

    thread_local! {
        static NEXT_AOT_COMPONENT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
        static NEXT_HOST_ELEMENT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
        static HOST_ELEMENTS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static SCROLL_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static SCROLL_PROP_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
        static CLICK_LISTENERS: std::cell::RefCell<std::collections::HashMap<u64, Value>> = std::cell::RefCell::new(std::collections::HashMap::new());
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

    pub fn dispatch_click(host_id: u64) {
        let listener = CLICK_LISTENERS.with(|listeners| listeners.borrow().get(&host_id).cloned());
        if let Some(listener) = listener {
            listener.call(
                Value::Undefined,
                vec![Value::object(std::collections::HashMap::new())],
            );
        }
    }

    pub fn has_pending_render() -> bool {
        super::has_dirty()
    }

    pub fn clear_pending_render() {
        let _ = super::take_dirty();
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
                        Component::button(label, style_from_props(&props))
                    }
                    "input" | "textarea" => Component::text_input(
                        props.get_property("value").to_js_string(),
                        props.get_property("placeholder").to_js_string(),
                        style_from_props(&props),
                    ),
                    _ => Component::boxed(style_from_props(&props), children),
                };
                let on_click = props.get_property("onClick");
                if on_click.is_function() {
                    CLICK_LISTENERS.with(|listeners| {
                        listeners.borrow_mut().insert(host_id, on_click);
                    });
                    component.on_click = w3cos_std::EventAction::NativeClick(host_id);
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
                if SCROLL_LISTENERS.with(|listeners| listeners.borrow().contains_key(&host_id))
                    || SCROLL_PROP_LISTENERS
                        .with(|listeners| listeners.borrow().contains_key(&host_id))
                {
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
        // Intrinsic HTML elements participate in normal flow unless their
        // parent explicitly establishes flex layout. The native host uses a
        // column flex box as its block-flow approximation, so allowing the
        // Taffy flex-item default (`flex-shrink: 1`) would collapse explicit
        // block heights. In particular, react-window's total-size spacer must
        // retain rowCount * rowHeight to define the scroll range.
        style.flex_shrink = 0.0;
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
            "row" => FlexDirection::Row,
            "row-reverse" => FlexDirection::RowReverse,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => FlexDirection::Column,
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
        style.text_align = match source.get_property("textAlign").to_js_string().as_str() {
            "center" => TextAlign::Center,
            "right" | "end" => TextAlign::Right,
            _ => TextAlign::Left,
        };
        style.white_space = match source.get_property("whiteSpace").to_js_string().as_str() {
            "nowrap" => WhiteSpace::NoWrap,
            "pre" => WhiteSpace::Pre,
            "pre-wrap" => WhiteSpace::PreWrap,
            _ => WhiteSpace::Normal,
        };
        style.text_overflow = if source.get_property("textOverflow").to_js_string() == "ellipsis" {
            TextOverflow::Ellipsis
        } else {
            TextOverflow::Clip
        };
        style.font_family = match source.get_property("fontFamily").to_js_string().as_str() {
            "" | "undefined" => None,
            family => Some(family.to_string()),
        };
        style.font_style = match source.get_property("fontStyle").to_js_string().as_str() {
            "italic" => FontStyle::Italic,
            "oblique" => FontStyle::Oblique,
            _ => FontStyle::Normal,
        };
        style.word_break = match source.get_property("wordBreak").to_js_string().as_str() {
            "break-all" => WordBreak::BreakAll,
            "break-word" => WordBreak::BreakWord,
            "keep-all" => WordBreak::KeepAll,
            _ => WordBreak::Normal,
        };
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
            _ => Position::Relative,
        };
        style.display = match source.get_property("display").to_js_string().as_str() {
            "none" => Display::None,
            "block" => Display::Block,
            "inline" => Display::Inline,
            "inline-block" => Display::InlineBlock,
            "grid" => Display::Grid,
            "flex" => Display::Flex,
            // Keep the established column-flow fallback until the native
            // retained painter supports block formatting contexts end to end.
            // Explicit `display: block` still uses the standards path.
            _ => Display::Flex,
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
        style.visibility = match source.get_property("visibility").to_js_string().as_str() {
            "hidden" => Visibility::Hidden,
            "collapse" => Visibility::Collapse,
            _ => Visibility::Visible,
        };
        style.user_select = match source.get_property("userSelect").to_js_string().as_str() {
            "none" => UserSelect::None,
            "text" => UserSelect::Text,
            "all" => UserSelect::All,
            _ => UserSelect::Auto,
        };
        style.cursor = match source.get_property("cursor").to_js_string().as_str() {
            "pointer" => Cursor::Pointer,
            "text" => Cursor::Text,
            "move" => Cursor::Move,
            "grab" => Cursor::Grab,
            "grabbing" => Cursor::Grabbing,
            "not-allowed" => Cursor::NotAllowed,
            "none" => Cursor::None,
            _ => Cursor::Default,
        };
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
        let value = value.to_js_string();
        (value.starts_with('#') || value == "transparent").then(|| Color::from_hex(&value))
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
        use std::cell::Cell;
        use std::rc::Rc;
        use w3cos_std::EventAction;
        use w3cos_std::component::ComponentKind;

        #[test]
        fn absolute_translate_y_positions_the_entire_host_subtree() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("position", Value::from("absolute"));
            style.set_property("transform", Value::from("translateY(76px)"));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style.clone());

            let native = style_from_props(&props);
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

            let native = style_from_props(&props);
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

            let native = style_from_props(&props);
            assert!(matches!(
                native.padding.top,
                Spacing::SafeAreaInset(w3cos_std::safe_area::SafeAreaEdge::Top)
            ));

            style.set_property(
                "paddingTop",
                Value::from("calc(18px + env(safe-area-inset-top))"),
            );
            let native = style_from_props(&props);
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
            let EventAction::NativeClick(host_id) = component.on_click else {
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
            let EventAction::NativeScroll(host_id) = component.on_click else {
                panic!("scroll host did not register its onScroll callback");
            };

            dispatch_scroll(host_id, 168.0);

            assert_eq!(observed.get(), 168.0);
        }

        #[test]
        fn explicit_height_host_node_does_not_shrink_like_a_flex_item() {
            let style = Value::object(std::collections::HashMap::new());
            style.set_property("height", Value::Number(84_000.0));
            let props = Value::object(std::collections::HashMap::new());
            props.set_property("style", style);

            let native = style_from_props(&props);
            assert!(matches!(native.height, Dimension::Px(value) if value == 84_000.0));
            assert_eq!(native.flex_shrink, 0.0);
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

            let native = style_from_props(&props);
            assert!(matches!(native.display, Display::Flex));
            assert!(matches!(native.margin.top, Spacing::Px(12.0)));
            assert_eq!(native.line_height, 1.5);
            assert_eq!(native.letter_spacing, 0.5);
            assert!(matches!(native.text_align, TextAlign::Center));
            assert!(matches!(native.white_space, WhiteSpace::NoWrap));
            let shadow = native.box_shadow.expect("box shadow should be mapped");
            assert_eq!(shadow.offset_y, 3.0);
            assert_eq!(shadow.blur_radius, 8.0);

            style.set_property("display", Value::from("flex"));
            assert!(matches!(style_from_props(&props).display, Display::Flex));
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

            let native = style_from_props(&props);
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

            let component = render_to_component(createElement(vec![Value::from("input"), props]));

            assert!(matches!(
                component.kind,
                ComponentKind::TextInput { ref value, ref placeholder }
                    if value == "上海" && placeholder == "请输入目的地"
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
