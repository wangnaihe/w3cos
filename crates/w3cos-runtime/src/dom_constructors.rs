//! DOM constructor identities and prototype chains for Value-backed nodes.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

pub const DOM_CONSTRUCTOR_NAMES: &[&str] = &[
    "Node",
    "Element",
    "HTMLElement",
    "HTMLAnchorElement",
    "HTMLDivElement",
    "HTMLSpanElement",
    "HTMLButtonElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
    "HTMLFormElement",
    "HTMLImageElement",
    "HTMLVideoElement",
    "HTMLCanvasElement",
    "SVGElement",
    "DocumentFragment",
    "Range",
    "Selection",
];

thread_local! {
    static CONSTRUCTORS: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
}

fn parent_name(name: &str) -> Option<&'static str> {
    match name {
        "Element" => Some("Node"),
        "HTMLElement" => Some("Element"),
        "SVGElement" => Some("Element"),
        "DocumentFragment" => Some("Node"),
        "HTMLAnchorElement"
        | "HTMLDivElement"
        | "HTMLSpanElement"
        | "HTMLButtonElement"
        | "HTMLInputElement"
        | "HTMLTextAreaElement"
        | "HTMLSelectElement"
        | "HTMLFormElement"
        | "HTMLImageElement"
        | "HTMLVideoElement"
        | "HTMLCanvasElement" => Some("HTMLElement"),
        _ => None,
    }
}

fn build_constructors() -> HashMap<String, Value> {
    let mut constructors = HashMap::new();
    for name in DOM_CONSTRUCTOR_NAMES {
        let constructor = if *name == "Range" {
            Value::function(|_, _| crate::jsdom::range_value(0, 0, 0, 0))
        } else {
            Value::function(|_, _| Value::Undefined)
        };
        constructor.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", constructor.clone());
        constructor.set_property("prototype", prototype);
        constructors.insert((*name).to_string(), constructor);
    }

    for name in DOM_CONSTRUCTOR_NAMES {
        let Some(parent) = parent_name(name) else {
            continue;
        };
        let prototype = constructors[*name].get_property("prototype");
        let parent_prototype = constructors[parent].get_property("prototype");
        w3cos_core::class::set_prototype_of(&prototype, &parent_prototype);
    }
    constructors
}

fn with_constructors<T>(read: impl FnOnce(&HashMap<String, Value>) -> T) -> T {
    CONSTRUCTORS.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(build_constructors());
        }
        read(
            slot.borrow()
                .as_ref()
                .expect("DOM constructors initialized"),
        )
    })
}

pub fn constructor(name: &str) -> Value {
    with_constructors(|constructors| constructors.get(name).cloned().unwrap_or(Value::Undefined))
}

pub fn prototype(name: &str) -> Value {
    constructor(name).get_property("prototype")
}

fn html_constructor_for_tag(tag: &str) -> &'static str {
    match tag {
        "a" => "HTMLAnchorElement",
        "div" => "HTMLDivElement",
        "span" => "HTMLSpanElement",
        "button" => "HTMLButtonElement",
        "input" => "HTMLInputElement",
        "textarea" => "HTMLTextAreaElement",
        "select" => "HTMLSelectElement",
        "form" => "HTMLFormElement",
        "img" => "HTMLImageElement",
        "video" => "HTMLVideoElement",
        "canvas" => "HTMLCanvasElement",
        _ => "HTMLElement",
    }
}

pub fn prototype_for_node(node_type: u16, tag: &str, is_svg: bool) -> Value {
    match node_type {
        1 if is_svg => prototype("SVGElement"),
        1 => prototype(html_constructor_for_tag(tag)),
        11 => prototype("DocumentFragment"),
        _ => prototype("Node"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_element_prototypes_follow_the_dom_hierarchy() {
        let div = Value::object(HashMap::new());
        w3cos_core::class::set_prototype_of(&div, &prototype_for_node(1, "div", false));
        assert!(w3cos_core::class::instance_of(
            &div,
            &constructor("HTMLDivElement")
        ));
        assert!(w3cos_core::class::instance_of(
            &div,
            &constructor("HTMLElement")
        ));
        assert!(w3cos_core::class::instance_of(
            &div,
            &constructor("Element")
        ));
        assert!(w3cos_core::class::instance_of(&div, &constructor("Node")));
        assert!(!w3cos_core::class::instance_of(
            &div,
            &constructor("HTMLSpanElement")
        ));
    }
}
