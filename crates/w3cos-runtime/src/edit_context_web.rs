//! EditContext text-model and geometry bridge.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::jsdom::realm_function;
use w3cos_core::Value;

struct EditState {
    text: String,
    selection_start: u32,
    selection_end: u32,
    character_bounds_range_start: u32,
    character_bounds: Value,
}

thread_local! {
    static EDIT_CONTEXT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TEXT_FORMAT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(message)],
    ))
}

fn index_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
        message,
        "IndexSizeError",
    ))
}

pub fn text_format_class() -> Value {
    TEXT_FORMAT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |this, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            this.set_property("rangeStart", init.get_property("rangeStart"));
            this.set_property("rangeEnd", init.get_property("rangeEnd"));
            this.set_property(
                "underlineStyle",
                Value::string(&init.get_property("underlineStyle").to_js_string()),
            );
            this.set_property(
                "underlineThickness",
                Value::string(&init.get_property("underlineThickness").to_js_string()),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("TextFormat"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "rangeEnd",
            "rangeStart",
            "underlineStyle",
            "underlineThickness",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn utf16_replace(text: &str, start: u32, end: u32, replacement: &str) -> String {
    let mut units = text.encode_utf16().collect::<Vec<_>>();
    if start > end || end as usize > units.len() {
        index_error("EditContext update range is outside the text");
    }
    units.splice(start as usize..end as usize, replacement.encode_utf16());
    String::from_utf16_lossy(&units)
}

fn edit_context_value(init: Value) -> Value {
    let generation = crate::jsdom::realm_generation();
    let text = {
        let value = init.get_property("text");
        if value.is_undefined() {
            String::new()
        } else {
            value.to_js_string()
        }
    };
    let length = text.encode_utf16().count() as u32;
    let selection_start = if init.get_property("selectionStart").is_undefined() {
        0
    } else {
        init.get_property("selectionStart").to_u32()
    };
    let selection_end = if init.get_property("selectionEnd").is_undefined() {
        selection_start
    } else {
        init.get_property("selectionEnd").to_u32()
    };
    if selection_start > selection_end || selection_end > length {
        index_error("EditContext selection is outside the text");
    }
    let state = Rc::new(RefCell::new(EditState {
        text,
        selection_start,
        selection_end,
        character_bounds_range_start: 0,
        character_bounds: Value::array(Vec::new()),
    }));
    let context =
        w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    context.set_property("attachedElements", Value::array(Vec::new()));
    for handler in [
        "oncharacterboundsupdate",
        "oncompositionend",
        "oncompositionstart",
        "ontextformatupdate",
        "ontextupdate",
    ] {
        context.set_property(handler, Value::Null);
    }
    for (member, getter) in [
        ("text", 0_u8),
        ("selectionStart", 1),
        ("selectionEnd", 2),
        ("characterBoundsRangeStart", 3),
        ("characterBounds", 4),
    ] {
        let getter_state = Rc::clone(&state);
        context.set_property(
            &format!("__w3cos_getter_{member}"),
            realm_function(generation, move |_, _| {
                let state = getter_state.borrow();
                match getter {
                    0 => Value::string(&state.text),
                    1 => Value::Number(state.selection_start as f64),
                    2 => Value::Number(state.selection_end as f64),
                    3 => Value::Number(state.character_bounds_range_start as f64),
                    _ => state.character_bounds.clone(),
                }
            }),
        );
    }
    let selection_state = Rc::clone(&state);
    context.set_property(
        "updateSelection",
        realm_function(generation, move |_, args| {
            let start = args.first().cloned().unwrap_or_default().to_u32();
            let end = args.get(1).cloned().unwrap_or_default().to_u32();
            let length = selection_state.borrow().text.encode_utf16().count() as u32;
            if start > end || end > length {
                index_error("EditContext selection is outside the text");
            }
            let mut state = selection_state.borrow_mut();
            state.selection_start = start;
            state.selection_end = end;
            Value::Undefined
        }),
    );
    let text_state = Rc::clone(&state);
    let text_context = context.clone();
    context.set_property(
        "updateText",
        realm_function(generation, move |_, args| {
            let start = args.first().cloned().unwrap_or_default().to_u32();
            let end = args.get(1).cloned().unwrap_or_default().to_u32();
            let replacement = args.get(2).cloned().unwrap_or_default().to_js_string();
            let replacement_length = replacement.encode_utf16().count() as u32;
            {
                let mut state = text_state.borrow_mut();
                state.text = utf16_replace(&state.text, start, end, &replacement);
                state.selection_start = start + replacement_length;
                state.selection_end = state.selection_start;
            }
            let event = w3cos_core::class::construct(
                &crate::web_events::event_subclass_class("TextUpdateEvent"),
                vec![
                    Value::string("textupdate"),
                    Value::object(HashMap::from([
                        ("text".into(), Value::string(&replacement)),
                        ("updateRangeStart".into(), Value::Number(start as f64)),
                        ("updateRangeEnd".into(), Value::Number(end as f64)),
                        (
                            "selectionStart".into(),
                            Value::Number((start + replacement_length) as f64),
                        ),
                        (
                            "selectionEnd".into(),
                            Value::Number((start + replacement_length) as f64),
                        ),
                    ])),
                ],
            );
            text_context.call_method("dispatchEvent", vec![event]);
            Value::Undefined
        }),
    );
    let bounds_state = Rc::clone(&state);
    context.set_property(
        "updateCharacterBounds",
        realm_function(generation, move |_, args| {
            let range_start = args.first().cloned().unwrap_or_default().to_u32();
            let bounds = args.get(1).cloned().unwrap_or(Value::Undefined);
            if !bounds.is_array() {
                type_error("updateCharacterBounds requires a sequence of DOMRectInit values");
            }
            let values = bounds
                .iter()
                .map(|rect| {
                    crate::geometry_web::rect(
                        rect.get_property("x").to_number(),
                        rect.get_property("y").to_number(),
                        rect.get_property("width").to_number(),
                        rect.get_property("height").to_number(),
                    )
                })
                .collect();
            let mut state = bounds_state.borrow_mut();
            state.character_bounds_range_start = range_start;
            state.character_bounds = Value::array(values);
            Value::Undefined
        }),
    );
    for method in ["updateControlBounds", "updateSelectionBounds"] {
        context.set_property(
            method,
            realm_function(generation, move |this, args| {
                let rect = args.first().cloned().unwrap_or(Value::Undefined);
                this.set_property(
                    &format!("__w3cos_{method}"),
                    crate::geometry_web::rect(
                        rect.get_property("x").to_number(),
                        rect.get_property("y").to_number(),
                        rect.get_property("width").to_number(),
                        rect.get_property("height").to_number(),
                    ),
                );
                Value::Undefined
            }),
        );
    }
    w3cos_core::class::set_prototype_of(&context, &edit_context_class().get_property("prototype"));
    context
}

pub fn edit_context_class() -> Value {
    EDIT_CONTEXT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, args| {
            edit_context_value(args.first().cloned().unwrap_or(Value::Undefined))
        });
        class.set_property("name", Value::string("EditContext"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "attachedElements",
            "characterBounds",
            "characterBoundsRangeStart",
            "oncharacterboundsupdate",
            "oncompositionend",
            "oncompositionstart",
            "ontextformatupdate",
            "ontextupdate",
            "selectionEnd",
            "selectionStart",
            "text",
            "updateCharacterBounds",
            "updateControlBounds",
            "updateSelection",
            "updateSelectionBounds",
            "updateText",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reset_realm() {
    EDIT_CONTEXT_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    TEXT_FORMAT_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
}

pub fn attach_element(context: &Value, element: Value) {
    if !w3cos_core::class::instance_of(context, &edit_context_class()) {
        type_error("HTMLElement.editContext must be an EditContext or null");
    }
    let mut elements = context
        .get_property("attachedElements")
        .iter()
        .collect::<Vec<_>>();
    if !elements.iter().any(|candidate| candidate == &element) {
        elements.push(element);
        context.set_property("attachedElements", Value::array(elements));
    }
}

pub fn detach_element(context: &Value, element: &Value) {
    let elements = context
        .get_property("attachedElements")
        .iter()
        .filter(|candidate| candidate != element)
        .collect();
    context.set_property("attachedElements", Value::array(elements));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn edit_context_updates_utf16_text_selection_bounds_and_events() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let context = w3cos_core::class::construct(
            &edit_context_class(),
            vec![Value::object(HashMap::from([
                ("text".into(), Value::string("a😀b")),
                ("selectionStart".into(), Value::Number(1.0)),
                ("selectionEnd".into(), Value::Number(3.0)),
            ]))],
        );
        let updates = Rc::new(Cell::new(0));
        let updates_for_listener = Rc::clone(&updates);
        context.call_method(
            "addEventListener",
            vec![
                Value::string("textupdate"),
                Value::function(move |_, args| {
                    assert_eq!(args[0].get_property("updateRangeStart").to_u32(), 1);
                    updates_for_listener.set(updates_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        context.call_method(
            "updateText",
            vec![Value::Number(1.0), Value::Number(3.0), Value::string("Z")],
        );
        assert_eq!(context.get_property("text").to_js_string(), "aZb");
        assert_eq!(context.get_property("selectionStart").to_u32(), 2);
        assert_eq!(updates.get(), 1);
        context.call_method(
            "updateCharacterBounds",
            vec![
                Value::Number(1.0),
                Value::array(vec![Value::object(HashMap::from([
                    ("x".into(), Value::Number(2.0)),
                    ("y".into(), Value::Number(3.0)),
                    ("width".into(), Value::Number(4.0)),
                    ("height".into(), Value::Number(5.0)),
                ]))]),
            ],
        );
        assert_eq!(
            context.get_property("characterBoundsRangeStart").to_u32(),
            1
        );
        assert_eq!(
            context
                .get_property("characterBounds")
                .get_property("0")
                .get_property("width")
                .to_number(),
            4.0
        );
        reset_realm();
    }

    #[test]
    fn text_model_entry_points_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_class = edit_context_class();
        let old_format_class = text_format_class();
        let context = w3cos_core::class::construct(
            &old_class,
            vec![Value::object(HashMap::from([(
                "text".into(),
                Value::string("old"),
            )]))],
        );
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_class = edit_context_class();
        let new_format_class = text_format_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(!old_format_class.strict_eq(&new_format_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(
            old_format_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );
        assert!(context.get_property("text").is_undefined());
        for method in [
            "updateSelection",
            "updateText",
            "updateCharacterBounds",
            "updateControlBounds",
            "updateSelectionBounds",
        ] {
            assert!(context.call_method(method, vec![]).is_undefined());
        }
        assert_eq!(
            w3cos_core::class::construct(&new_class, vec![])
                .get_property("text")
                .to_js_string(),
            ""
        );
        reset_realm();
    }
}
