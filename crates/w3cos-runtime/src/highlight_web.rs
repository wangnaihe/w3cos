//! CSS Custom Highlight API data model.
//!
//! Range registration is fully usable. Painting registered ranges is a
//! compositor integration boundary and is reported once when the registry is
//! mutated.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static HIGHLIGHT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static HIGHLIGHT_REGISTRY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static HIGHLIGHT_REGISTRY: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(message)],
    ))
}

fn warn_paint_pending() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: CSS.highlights preserves registry and range semantics; \
             custom-highlight painting requires compositor range-decoration integration"
        );
    });
}

fn abstract_range(value: &Value) -> bool {
    w3cos_core::class::instance_of(
        value,
        &crate::dom_constructors::constructor("AbstractRange"),
    )
}

fn range_array(ranges: &[Value]) -> Value {
    Value::array(ranges.to_vec())
}

pub fn highlight_class() -> Value {
    HIGHLIGHT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |this, args| {
            if args.iter().any(|range| !abstract_range(range)) {
                type_error("Highlight entries must implement AbstractRange");
            }
            let ranges = Rc::new(RefCell::new(args));
            this.set_property("priority", Value::Number(0.0));
            this.set_property("type", Value::string("highlight"));
            this.set_property(
                "add",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |this, args| {
                        let range = arg(&args, 0);
                        if !abstract_range(&range) {
                            type_error("Highlight.add expects an AbstractRange");
                        }
                        if !ranges.borrow().iter().any(|item| item == &range) {
                            ranges.borrow_mut().push(range);
                        }
                        this
                    }
                }),
            );
            this.set_property(
                "clear",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |_, _| {
                        ranges.borrow_mut().clear();
                        Value::Undefined
                    }
                }),
            );
            this.set_property(
                "delete",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |_, args| {
                        let range = arg(&args, 0);
                        let mut ranges = ranges.borrow_mut();
                        let previous = ranges.len();
                        ranges.retain(|item| item != &range);
                        Value::Bool(previous != ranges.len())
                    }
                }),
            );
            this.set_property(
                "has",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |_, args| {
                        Value::Bool(ranges.borrow().iter().any(|item| item == &arg(&args, 0)))
                    }
                }),
            );
            for name in ["keys", "values"] {
                this.set_property(
                    name,
                    realm_function(generation, {
                        let ranges = Rc::clone(&ranges);
                        move |_, _| range_array(&ranges.borrow())
                    }),
                );
            }
            this.set_property(
                "entries",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |_, _| {
                        Value::array(
                            ranges
                                .borrow()
                                .iter()
                                .map(|range| Value::array(vec![range.clone(), range.clone()]))
                                .collect(),
                        )
                    }
                }),
            );
            this.set_property(
                "forEach",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |this, args| {
                        let callback = arg(&args, 0);
                        if !callback.is_function() {
                            type_error("Highlight.forEach callback must be callable");
                        }
                        for range in ranges.borrow().iter() {
                            callback.call(
                                arg(&args, 1),
                                vec![range.clone(), range.clone(), this.clone()],
                            );
                        }
                        Value::Undefined
                    }
                }),
            );
            this.set_property(
                "__w3cos_getter_size",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |_, _| Value::Number(ranges.borrow().len() as f64)
                }),
            );
            this.set_property(
                "__w3cos_ranges_snapshot",
                realm_function(generation, {
                    let ranges = Rc::clone(&ranges);
                    move |_, _| range_array(&ranges.borrow())
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("Highlight"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "add", "clear", "delete", "entries", "forEach", "has", "keys", "priority", "size",
            "type", "values",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn highlight_registry_class() -> Value {
    HIGHLIGHT_REGISTRY_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            type_error("Illegal constructor: HighlightRegistry")
        });
        class.set_property("name", Value::string("HighlightRegistry"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "clear",
            "delete",
            "entries",
            "forEach",
            "get",
            "has",
            "highlightsFromPoint",
            "keys",
            "set",
            "size",
            "values",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn highlight_registry_value() -> Value {
    HIGHLIGHT_REGISTRY.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let generation = crate::jsdom::realm_generation();
        let entries = Rc::new(RefCell::new(HashMap::<String, Value>::new()));
        let value = Value::object(HashMap::new());
        value.set_property(
            "set",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |this, args| {
                    let name = arg(&args, 0).to_js_string();
                    let highlight = arg(&args, 1);
                    if !w3cos_core::class::instance_of(&highlight, &highlight_class()) {
                        type_error("HighlightRegistry.set expects a Highlight");
                    }
                    entries.borrow_mut().insert(name, highlight);
                    warn_paint_pending();
                    this
                }
            }),
        );
        value.set_property(
            "get",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, args| {
                    entries
                        .borrow()
                        .get(&arg(&args, 0).to_js_string())
                        .cloned()
                        .unwrap_or(Value::Undefined)
                }
            }),
        );
        value.set_property(
            "has",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, args| {
                    Value::Bool(entries.borrow().contains_key(&arg(&args, 0).to_js_string()))
                }
            }),
        );
        value.set_property(
            "delete",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, args| {
                    let removed = entries
                        .borrow_mut()
                        .remove(&arg(&args, 0).to_js_string())
                        .is_some();
                    if removed {
                        warn_paint_pending();
                    }
                    Value::Bool(removed)
                }
            }),
        );
        value.set_property(
            "clear",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, _| {
                    if !entries.borrow().is_empty() {
                        entries.borrow_mut().clear();
                        warn_paint_pending();
                    }
                    Value::Undefined
                }
            }),
        );
        value.set_property(
            "keys",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, _| {
                    Value::array(
                        entries
                            .borrow()
                            .keys()
                            .map(|name| Value::string(name))
                            .collect(),
                    )
                }
            }),
        );
        value.set_property(
            "values",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, _| Value::array(entries.borrow().values().cloned().collect())
            }),
        );
        value.set_property(
            "entries",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, _| {
                    Value::array(
                        entries
                            .borrow()
                            .iter()
                            .map(|(name, highlight)| {
                                Value::array(vec![Value::string(name), highlight.clone()])
                            })
                            .collect(),
                    )
                }
            }),
        );
        value.set_property(
            "forEach",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |this, args| {
                    let callback = arg(&args, 0);
                    if !callback.is_function() {
                        type_error("HighlightRegistry.forEach callback must be callable");
                    }
                    for (name, highlight) in entries.borrow().iter() {
                        callback.call(
                            arg(&args, 1),
                            vec![highlight.clone(), Value::string(name), this.clone()],
                        );
                    }
                    Value::Undefined
                }
            }),
        );
        value.set_property(
            "highlightsFromPoint",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, args| {
                    let x = arg(&args, 0).to_number();
                    let y = arg(&args, 1).to_number();
                    if !x.is_finite() || !y.is_finite() {
                        type_error(
                            "HighlightRegistry.highlightsFromPoint expects finite coordinates",
                        );
                    }
                    let mut hits = Vec::new();
                    for highlight in entries.borrow().values() {
                        let ranges = highlight.call_method("__w3cos_ranges_snapshot", vec![]);
                        let matching = ranges
                            .iter()
                            .filter(|range| {
                                let rect = range.call_method("getBoundingClientRect", vec![]);
                                let left = rect.get_property("left").to_number();
                                let right = rect.get_property("right").to_number();
                                let top = rect.get_property("top").to_number();
                                let bottom = rect.get_property("bottom").to_number();
                                x >= left && x <= right && y >= top && y <= bottom
                            })
                            .collect::<Vec<_>>();
                        if !matching.is_empty() {
                            hits.push(Value::object(HashMap::from([
                                ("highlight".to_string(), highlight.clone()),
                                ("ranges".to_string(), Value::array(matching)),
                            ])));
                        }
                    }
                    Value::array(hits)
                }
            }),
        );
        value.set_property(
            "__w3cos_getter_size",
            realm_function(generation, {
                let entries = Rc::clone(&entries);
                move |_, _| Value::Number(entries.borrow().len() as f64)
            }),
        );
        w3cos_core::class::set_prototype_of(
            &value,
            &highlight_registry_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn reset() {
    HIGHLIGHT_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    HIGHLIGHT_REGISTRY_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    HIGHLIGHT_REGISTRY.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_and_registry_keep_set_and_map_semantics() {
        let range = crate::jsdom::range_value(0, 0, 0, 0);
        let highlight = w3cos_core::class::construct(&highlight_class(), vec![range.clone()]);
        assert_eq!(highlight.get_property("size").to_number(), 1.0);
        assert!(highlight.call_method("has", vec![range]).to_bool());

        let registry = highlight_registry_value();
        registry.call_method(
            "set",
            vec![Value::string("search-result"), highlight.clone()],
        );
        assert_eq!(registry.get_property("size").to_number(), 1.0);
        assert!(registry.call_method("get", vec![Value::string("search-result")]) == highlight);
        assert!(
            registry
                .call_method("delete", vec![Value::string("search-result")])
                .to_bool()
        );
    }

    #[test]
    fn highlights_and_registry_are_realm_owned() {
        reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_range = crate::jsdom::range_value(0, 0, 0, 0);
        let old_class = highlight_class();
        let old_registry_class = highlight_registry_class();
        let old_highlight = w3cos_core::class::construct(&old_class, vec![old_range.clone()]);
        let old_registry = highlight_registry_value();
        old_registry.call_method(
            "set",
            vec![Value::string("old-result"), old_highlight.clone()],
        );
        old_registry_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let new_class = highlight_class();
        let new_registry_class = highlight_registry_class();
        let new_registry = highlight_registry_value();
        assert!(old_class != new_class);
        assert!(old_registry_class != new_registry_class);
        assert!(old_registry != new_registry);
        assert!(
            new_registry_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert_eq!(new_registry.get_property("size").to_number(), 0.0);
        assert!(
            old_registry
                .call_method("get", vec![Value::string("old-result")])
                .is_undefined()
        );
        assert!(
            old_highlight
                .call_method("has", vec![old_range])
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());

        let new_range = crate::jsdom::range_value(0, 0, 0, 0);
        let new_highlight = w3cos_core::class::construct(&new_class, vec![new_range.clone()]);
        assert!(new_highlight.call_method("has", vec![new_range]).to_bool());
        new_registry.call_method(
            "set",
            vec![Value::string("new-result"), new_highlight.clone()],
        );
        assert!(
            new_registry.call_method("get", vec![Value::string("new-result")]) == new_highlight
        );
        reset();
    }
}
