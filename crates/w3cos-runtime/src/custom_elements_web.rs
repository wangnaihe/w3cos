//! JavaScript-facing Custom Elements registry and lifecycle bridge.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, realm_function, register_weak_realm_object, reset_realm_class,
    upgrade_realm_object,
};

thread_local! {
    static DEFINITIONS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static WAITERS: RefCell<HashMap<String, Vec<Value>>> = RefCell::new(HashMap::new());
    static REGISTRY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static REGISTRY_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static LIFECYCLE_WARNING_EMITTED: RefCell<bool> = const { RefCell::new(false) };
    static ELEMENT_INTERNALS_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CUSTOM_STATE_SET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CSS_PSEUDO_ELEMENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ELEMENT_INTERNALS_VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static CUSTOM_STATE_SET_VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static CSS_PSEUDO_ELEMENT_VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
}

const ARIA_STRING_MEMBERS: &[&str] = &[
    "ariaAtomic",
    "ariaAutoComplete",
    "ariaBrailleLabel",
    "ariaBrailleRoleDescription",
    "ariaBusy",
    "ariaChecked",
    "ariaColCount",
    "ariaColIndex",
    "ariaColIndexText",
    "ariaColSpan",
    "ariaCurrent",
    "ariaDescription",
    "ariaDisabled",
    "ariaExpanded",
    "ariaHasPopup",
    "ariaHidden",
    "ariaInvalid",
    "ariaKeyShortcuts",
    "ariaLabel",
    "ariaLevel",
    "ariaLive",
    "ariaModal",
    "ariaMultiLine",
    "ariaMultiSelectable",
    "ariaOrientation",
    "ariaPlaceholder",
    "ariaPosInSet",
    "ariaPressed",
    "ariaReadOnly",
    "ariaRelevant",
    "ariaRequired",
    "ariaRoleDescription",
    "ariaRowCount",
    "ariaRowIndex",
    "ariaRowIndexText",
    "ariaRowSpan",
    "ariaSelected",
    "ariaSetSize",
    "ariaSort",
    "ariaValueMax",
    "ariaValueMin",
    "ariaValueNow",
    "ariaValueText",
    "role",
];

const ARIA_ELEMENT_MEMBERS: &[&str] = &[
    "ariaActiveDescendantElement",
    "ariaControlsElements",
    "ariaDescribedByElements",
    "ariaDetailsElements",
    "ariaErrorMessageElements",
    "ariaFlowToElements",
    "ariaLabelledByElements",
];

fn exception(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn realm_is_current(generation: u32) -> bool {
    crate::jsdom::realm_generation() == generation
}

fn realm_custom_elements_function(
    callback: impl Fn(Value, Vec<Value>) -> Value + 'static,
) -> Value {
    realm_function(crate::jsdom::realm_generation(), callback)
}

pub fn custom_state_set_class() -> Value {
    CUSTOM_STATE_SET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_custom_elements_function(|_, _| {
            w3cos_core::throw_value(exception(
                "TypeError",
                "Illegal constructor: CustomStateSet",
            ))
        });
        class.set_property("name", Value::string("CustomStateSet"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "add", "clear", "delete", "entries", "forEach", "has", "keys", "size", "values",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn custom_state_set_value() -> Value {
    let states = Rc::new(RefCell::new(BTreeSet::<String>::new()));
    let value = Value::object(HashMap::new());
    let add_states = Rc::clone(&states);
    let add_value = value.clone();
    value.set_property(
        "add",
        realm_custom_elements_function(move |_, args| {
            let state = args.first().cloned().unwrap_or_default().to_js_string();
            if !state.starts_with("--") || state.len() <= 2 {
                w3cos_core::throw_value(exception(
                    "SyntaxError",
                    "custom state names must be non-empty dashed identifiers",
                ));
            }
            add_states.borrow_mut().insert(state);
            add_value.clone()
        }),
    );
    let clear_states = Rc::clone(&states);
    value.set_property(
        "clear",
        realm_custom_elements_function(move |_, _| {
            clear_states.borrow_mut().clear();
            Value::Undefined
        }),
    );
    let delete_states = Rc::clone(&states);
    value.set_property(
        "delete",
        realm_custom_elements_function(move |_, args| {
            Value::Bool(
                delete_states
                    .borrow_mut()
                    .remove(&args.first().cloned().unwrap_or_default().to_js_string()),
            )
        }),
    );
    let has_states = Rc::clone(&states);
    value.set_property(
        "has",
        realm_custom_elements_function(move |_, args| {
            Value::Bool(
                has_states
                    .borrow()
                    .contains(&args.first().cloned().unwrap_or_default().to_js_string()),
            )
        }),
    );
    let size_states = Rc::clone(&states);
    value.set_property(
        "__w3cos_getter_size",
        realm_custom_elements_function(
            move |_, _| Value::Number(size_states.borrow().len() as f64),
        ),
    );
    for method in ["keys", "values", "entries"] {
        let iterator_states = Rc::clone(&states);
        value.set_property(
            method,
            realm_custom_elements_function(move |_, _| {
                let entries = iterator_states
                    .borrow()
                    .iter()
                    .map(|state| {
                        if method == "entries" {
                            Value::array(vec![Value::string(state), Value::string(state)])
                        } else {
                            Value::string(state)
                        }
                    })
                    .collect();
                Value::array(entries).call_method("__w3cos_symbol_iterator", Vec::new())
            }),
        );
    }
    let each_states = Rc::clone(&states);
    let each_value = value.clone();
    value.set_property(
        "forEach",
        realm_custom_elements_function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                w3cos_core::throw_value(exception(
                    "TypeError",
                    "CustomStateSet.forEach requires a callback",
                ));
            }
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            for state in each_states.borrow().iter() {
                callback.call(
                    this_arg.clone(),
                    vec![
                        Value::string(state),
                        Value::string(state),
                        each_value.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &custom_state_set_class().get_property("prototype"),
    );
    register_weak_realm_object(&CUSTOM_STATE_SET_VALUES, &value);
    value
}

pub fn element_internals_class() -> Value {
    ELEMENT_INTERNALS_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_custom_elements_function(|_, _| {
            w3cos_core::throw_value(exception(
                "TypeError",
                "Illegal constructor: ElementInternals",
            ))
        });
        class.set_property("name", Value::string("ElementInternals"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ARIA_STRING_MEMBERS
            .iter()
            .chain(ARIA_ELEMENT_MEMBERS)
            .chain(
                [
                    "checkValidity",
                    "form",
                    "labels",
                    "reportValidity",
                    "setFormValue",
                    "setValidity",
                    "shadowRoot",
                    "states",
                    "validationMessage",
                    "validity",
                    "willValidate",
                ]
                .iter(),
            )
        {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn validity_value(valid: bool, custom_error: bool) -> Value {
    let value = Value::object(HashMap::new());
    for member in [
        "badInput",
        "customError",
        "patternMismatch",
        "rangeOverflow",
        "rangeUnderflow",
        "stepMismatch",
        "tooLong",
        "tooShort",
        "typeMismatch",
        "valid",
        "valueMissing",
    ] {
        value.set_property(
            member,
            Value::Bool(if member == "valid" {
                valid
            } else {
                member == "customError" && custom_error
            }),
        );
    }
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("ValidityState"),
    );
    value
}

fn form_owner(node: u32) -> Value {
    let mut parent = crate::dom::parent_node(node);
    while let Some(candidate) = parent {
        if crate::dom::tag_name(candidate) == "form" {
            return crate::jsdom::element_value(candidate);
        }
        parent = crate::dom::parent_node(candidate);
    }
    Value::Null
}

pub fn element_internals_value(element: Value, node: u32) -> Value {
    let internals = Value::object(HashMap::new());
    for member in ARIA_STRING_MEMBERS {
        internals.set_property(member, Value::Null);
    }
    for member in ARIA_ELEMENT_MEMBERS {
        internals.set_property(
            member,
            if *member == "ariaActiveDescendantElement" {
                Value::Null
            } else {
                Value::array(Vec::new())
            },
        );
    }
    internals.set_property("form", form_owner(node));
    internals.set_property("labels", Value::array(Vec::new()));
    internals.set_property("shadowRoot", element.get_property("shadowRoot"));
    internals.set_property("states", custom_state_set_value());
    internals.set_property("validationMessage", Value::string(""));
    internals.set_property("validity", validity_value(true, false));
    internals.set_property("willValidate", Value::Bool(true));
    let validity_element = element.clone();
    let validity_internals = internals.clone();
    internals.set_property(
        "setValidity",
        realm_custom_elements_function(move |_, args| {
            let flags = args.first().cloned().unwrap_or(Value::Undefined);
            let message = args
                .get(1)
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let invalid = [
                "badInput",
                "customError",
                "patternMismatch",
                "rangeOverflow",
                "rangeUnderflow",
                "stepMismatch",
                "tooLong",
                "tooShort",
                "typeMismatch",
                "valueMissing",
            ]
            .iter()
            .any(|flag| flags.get_property(flag).to_bool());
            if invalid && (message.is_empty() || message == "undefined") {
                w3cos_core::throw_value(exception(
                    "TypeError",
                    "setValidity requires a message when a validity flag is true",
                ));
            }
            validity_internals.set_property(
                "validationMessage",
                Value::string(if invalid { &message } else { "" }),
            );
            validity_internals.set_property(
                "validity",
                validity_value(!invalid, flags.get_property("customError").to_bool()),
            );
            validity_element.set_property("__w3cos_internals_invalid", Value::Bool(invalid));
            Value::Undefined
        }),
    );
    let check_element = element.clone();
    let check = realm_custom_elements_function(move |_, _| {
        let valid = !check_element
            .get_property("__w3cos_internals_invalid")
            .to_bool();
        if !valid {
            let event = w3cos_core::class::construct(
                &crate::web_events::event_class(),
                vec![
                    Value::string("invalid"),
                    Value::object(HashMap::from([("cancelable".into(), Value::Bool(true))])),
                ],
            );
            check_element.call_method("dispatchEvent", vec![event]);
        }
        Value::Bool(valid)
    });
    internals.set_property("checkValidity", check.clone());
    internals.set_property(
        "reportValidity",
        realm_custom_elements_function(move |this, _| {
            static WARNING: std::sync::Once = std::sync::Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: ElementInternals.reportValidity dispatches invalid events; \
                     native validation UI requires a host form adapter"
                );
            });
            this.call_method("checkValidity", Vec::new())
        }),
    );
    let form_element = element;
    internals.set_property(
        "setFormValue",
        realm_custom_elements_function(move |_, args| {
            form_element.set_property(
                "__w3cos_form_value",
                args.first().cloned().unwrap_or(Value::Null),
            );
            form_element.set_property(
                "__w3cos_form_state",
                args.get(1).cloned().unwrap_or(Value::Null),
            );
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &internals,
        &element_internals_class().get_property("prototype"),
    );
    register_weak_realm_object(&ELEMENT_INTERNALS_VALUES, &internals);
    internals
}

pub fn css_pseudo_element_class() -> Value {
    CSS_PSEUDO_ELEMENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_custom_elements_function(|_, _| {
            w3cos_core::throw_value(exception(
                "TypeError",
                "Illegal constructor: CSSPseudoElement",
            ))
        });
        class.set_property("name", Value::string("CSSPseudoElement"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ["element", "parent", "pseudo", "type"] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn css_pseudo_element_value(element: Value, parent: Value, pseudo_type: String) -> Value {
    if !pseudo_type.starts_with("::") {
        w3cos_core::throw_value(exception(
            "SyntaxError",
            "pseudo element selectors must start with '::'",
        ));
    }
    let value = Value::object(HashMap::from([
        ("element".into(), element.clone()),
        ("parent".into(), parent),
        ("type".into(), Value::string(&pseudo_type)),
    ]));
    let nested_element = element;
    let nested_parent = value.clone();
    value.set_property(
        "pseudo",
        realm_custom_elements_function(move |_, args| {
            css_pseudo_element_value(
                nested_element.clone(),
                nested_parent.clone(),
                args.first().cloned().unwrap_or_default().to_js_string(),
            )
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &css_pseudo_element_class().get_property("prototype"),
    );
    register_weak_realm_object(&CSS_PSEUDO_ELEMENT_VALUES, &value);
    value
}

fn valid_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name == lower
        && lower.contains('-')
        && !lower.starts_with("xml")
        && !matches!(
            lower.as_str(),
            "annotation-xml"
                | "color-profile"
                | "font-face"
                | "font-face-src"
                | "font-face-uri"
                | "font-face-format"
                | "font-face-name"
                | "missing-glyph"
        )
        && lower.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        })
}

fn invoke_callback(element: &Value, name: &str, args: Vec<Value>) {
    let callback = element.get_property(name);
    if callback.is_function() {
        callback.call(element.clone(), args);
    }
}

fn run_constructor(element: &Value, constructor: &Value) {
    let prototype = constructor.get_property("prototype");
    if prototype.is_object() {
        w3cos_core::class::set_prototype_of(element, &prototype);
    }
    let raw = constructor.get_property("__w3cos_ctor");
    if raw.is_function() {
        raw.call(element.clone(), vec![]);
    } else if constructor.is_function() {
        constructor.call(element.clone(), vec![]);
    }
}

/// Upgrade an element created through the document bridge when its tag has a
/// registered autonomous custom-element definition.
pub fn upgrade_created_element(tag: &str, element: Value) -> Value {
    if element
        .get_property("__w3cos_custom_element_upgraded")
        .to_bool()
    {
        return element;
    }
    let constructor = DEFINITIONS.with(|definitions| definitions.borrow().get(tag).cloned());
    if let Some(constructor) = constructor {
        run_constructor(&element, &constructor);
        element.set_property("__w3cos_custom_element_upgraded", Value::Bool(true));
    }
    element
}

/// Deliver the connected callback after a DOM mutation makes an upgraded
/// element connected.
pub fn connected(element: &Value) {
    if element
        .get_property("__w3cos_custom_element_upgraded")
        .to_bool()
    {
        invoke_callback(element, "connectedCallback", vec![]);
    }
}

/// Deliver the disconnected callback after removing an upgraded element.
pub fn disconnected(element: &Value) {
    if element
        .get_property("__w3cos_custom_element_upgraded")
        .to_bool()
    {
        invoke_callback(element, "disconnectedCallback", vec![]);
    }
}

pub fn connected_subtree(root: &Value) {
    lifecycle_subtree(root, true);
}

pub fn disconnected_subtree(root: &Value) {
    lifecycle_subtree(root, false);
}

fn lifecycle_subtree(root: &Value, is_connected: bool) {
    let root_id = root.get_property("__node_id");
    if root_id.is_undefined() {
        return;
    }
    let mut pending = vec![root_id.to_u32()];
    while let Some(node) = pending.pop() {
        let element = crate::jsdom::element_value(node);
        if is_connected {
            connected(&element);
        } else {
            disconnected(&element);
        }
        pending.extend(crate::dom::children(node));
    }
}

fn upgrade_subtree(root: &Value) {
    let Some(root_id) = root
        .get_property("__node_id")
        .to_number()
        .is_finite()
        .then(|| root.get_property("__node_id").to_u32())
    else {
        return;
    };
    let mut pending = vec![root_id];
    while let Some(node) = pending.pop() {
        if crate::dom::node_type(node) == 1 {
            let tag = crate::dom::tag_name(node);
            let element = crate::jsdom::element_value(node);
            upgrade_created_element(&tag, element.clone());
            if crate::dom::is_connected(node) {
                connected(&element);
            }
        }
        pending.extend(crate::dom::children(node));
    }
}

pub fn custom_element_registry_class() -> Value {
    REGISTRY_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_custom_elements_function(move |_, _| {
            if realm_is_current(generation) {
                custom_elements_value()
            } else {
                Value::Undefined
            }
        });
        class.set_property("name", Value::string("CustomElementRegistry"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for name in [
            "define",
            "get",
            "getName",
            "whenDefined",
            "upgrade",
            "initialize",
        ] {
            prototype.set_property(name, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn custom_elements_value() -> Value {
    REGISTRY_VALUE.with(|slot| {
        if let Some(registry) = slot.borrow().clone() {
            return registry;
        }
        let generation = crate::jsdom::realm_generation();
        let define_generation = generation;
        let get_generation = generation;
        let get_name_generation = generation;
        let when_defined_generation = generation;
        let upgrade_generation = generation;
        let registry = Value::object(HashMap::from([
            (
                "define".into(),
                realm_custom_elements_function(move |_, args| {
                    if !realm_is_current(define_generation) {
                        return Value::Undefined;
                    }
                    let name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    let constructor = args.get(1).cloned().unwrap_or(Value::Undefined);
                    if !valid_name(&name) {
                        w3cos_core::throw_value(exception(
                            "SyntaxError",
                            "custom element names must be lowercase and contain a hyphen",
                        ));
                    }
                    if !constructor.is_function() && !constructor.is_object() {
                        w3cos_core::throw_value(exception(
                            "TypeError",
                            "custom element constructor must be callable",
                        ));
                    }
                    let duplicate = DEFINITIONS.with(|definitions| {
                        let definitions = definitions.borrow();
                        definitions.contains_key(&name)
                            || definitions
                                .values()
                                .any(|value| value.strict_eq(&constructor))
                    });
                    if duplicate {
                        w3cos_core::throw_value(exception(
                            "NotSupportedError",
                            "custom element name or constructor is already registered",
                        ));
                    }
                    DEFINITIONS.with(|definitions| {
                        definitions
                            .borrow_mut()
                            .insert(name.clone(), constructor.clone());
                    });
                    for resolve in WAITERS
                        .with(|waiters| waiters.borrow_mut().remove(&name))
                        .unwrap_or_default()
                    {
                        resolve.call(Value::Undefined, vec![constructor.clone()]);
                    }
                    Value::Undefined
                }),
            ),
            (
                "get".into(),
                realm_custom_elements_function(move |_, args| {
                    if !realm_is_current(get_generation) {
                        return Value::Undefined;
                    }
                    let name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    DEFINITIONS
                        .with(|definitions| definitions.borrow().get(&name).cloned())
                        .unwrap_or(Value::Undefined)
                }),
            ),
            (
                "getName".into(),
                realm_custom_elements_function(move |_, args| {
                    if !realm_is_current(get_name_generation) {
                        return Value::Undefined;
                    }
                    let constructor = args.first().cloned().unwrap_or(Value::Undefined);
                    DEFINITIONS
                        .with(|definitions| {
                            definitions.borrow().iter().find_map(|(name, value)| {
                                value.strict_eq(&constructor).then(|| Value::string(name))
                            })
                        })
                        .unwrap_or(Value::Null)
                }),
            ),
            (
                "whenDefined".into(),
                realm_custom_elements_function(move |_, args| {
                    if !realm_is_current(when_defined_generation) {
                        return Value::Undefined;
                    }
                    let name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    if !valid_name(&name) {
                        return w3cos_core::promise::reject(vec![exception(
                            "SyntaxError",
                            "custom element names must be lowercase and contain a hyphen",
                        )]);
                    }
                    if let Some(constructor) =
                        DEFINITIONS.with(|definitions| definitions.borrow().get(&name).cloned())
                    {
                        return w3cos_core::promise::resolve(vec![constructor]);
                    }
                    w3cos_core::promise::new(vec![realm_custom_elements_function({
                        move |_, args| {
                            if !realm_is_current(when_defined_generation) {
                                return Value::Undefined;
                            }
                            let resolve = args.first().cloned().unwrap_or(Value::Undefined);
                            WAITERS.with(|waiters| {
                                waiters
                                    .borrow_mut()
                                    .entry(name.clone())
                                    .or_default()
                                    .push(resolve);
                            });
                            Value::Undefined
                        }
                    })])
                }),
            ),
            (
                "upgrade".into(),
                realm_custom_elements_function(move |_, args| {
                    if !realm_is_current(upgrade_generation) {
                        return Value::Undefined;
                    }
                    LIFECYCLE_WARNING_EMITTED.with(|warned| {
                        if !warned.replace(true) {
                            eprintln!(
                                "[w3cos] warning: customElements upgrades autonomous custom \
                                 elements; customized built-ins and exact reaction-queue timing \
                                 remain pending"
                            );
                        }
                    });
                    upgrade_subtree(args.first().unwrap_or(&Value::Undefined));
                    Value::Undefined
                }),
            ),
        ]));
        let initialize_generation = generation;
        registry.set_property(
            "initialize",
            realm_custom_elements_function(move |_, _| {
                if !realm_is_current(initialize_generation) {
                    return Value::Undefined;
                }
                static WARNING: std::sync::Once = std::sync::Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: CustomElementRegistry.initialize is exposed as a \
                         compatibility no-op; scoped registry initialization remains pending"
                    );
                });
                Value::Undefined
            }),
        );
        w3cos_core::class::set_prototype_of(
            &registry,
            &custom_element_registry_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(registry.clone());
        registry
    })
}

pub fn reset() {
    DEFINITIONS.with(|definitions| definitions.borrow_mut().clear());
    WAITERS.with(|waiters| waiters.borrow_mut().clear());
    REGISTRY_VALUE.with(|slot| {
        if let Some(registry) = slot.borrow_mut().take() {
            for method in [
                "define",
                "get",
                "getName",
                "initialize",
                "upgrade",
                "whenDefined",
            ] {
                registry.set_property(method, Value::Undefined);
            }
        }
    });
    ELEMENT_INTERNALS_VALUES.with(|values| {
        for internals in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            for reference in ["form", "labels", "shadowRoot", "states", "validity"] {
                internals.set_property(reference, Value::Undefined);
            }
            for member in ARIA_ELEMENT_MEMBERS {
                internals.set_property(member, Value::Undefined);
            }
            for method in [
                "checkValidity",
                "reportValidity",
                "setFormValue",
                "setValidity",
            ] {
                internals.set_property(method, Value::Undefined);
            }
        }
    });
    CUSTOM_STATE_SET_VALUES.with(|values| {
        for states in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            for method in [
                "__w3cos_getter_size",
                "add",
                "clear",
                "delete",
                "entries",
                "forEach",
                "has",
                "keys",
                "values",
            ] {
                states.set_property(method, Value::Undefined);
            }
        }
    });
    CSS_PSEUDO_ELEMENT_VALUES.with(|values| {
        for pseudo in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            for reference in ["element", "parent", "pseudo"] {
                pseudo.set_property(reference, Value::Undefined);
            }
        }
    });
    reset_realm_class(&REGISTRY_CLASS);
    reset_realm_class(&ELEMENT_INTERNALS_CLASS);
    reset_realm_class(&CUSTOM_STATE_SET_CLASS);
    reset_realm_class(&CSS_PSEUDO_ELEMENT_CLASS);
    LIFECYCLE_WARNING_EMITTED.with(|warned| *warned.borrow_mut() = false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn registry_defines_upgrades_and_resolves_waiters() {
        reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let registry = custom_elements_value();
        let resolved = Rc::new(Cell::new(false));
        let resolved_for_callback = Rc::clone(&resolved);
        registry
            .call_method("whenDefined", vec![Value::string("x-panel")])
            .call_method(
                "then",
                vec![Value::function(move |_, _| {
                    resolved_for_callback.set(true);
                    Value::Undefined
                })],
            );
        let constructor = Value::function(|this, _| {
            this.set_property("ready", Value::Bool(true));
            Value::Undefined
        });
        let prototype = Value::object(HashMap::from([(
            "connectedCallback".into(),
            Value::function(|this, _| {
                this.set_property("connected", Value::Bool(true));
                Value::Undefined
            }),
        )]));
        constructor.set_property("prototype", prototype);
        registry.call_method(
            "define",
            vec![Value::string("x-panel"), constructor.clone()],
        );
        w3cos_core::promise::drain_microtasks();
        assert!(resolved.get());
        assert!(
            registry
                .call_method("get", vec![Value::string("x-panel")])
                .strict_eq(&constructor)
        );
        let element = crate::jsdom::document_value()
            .call_method("createElement", vec![Value::string("x-panel")]);
        assert!(element.get_property("ready").to_bool());
        assert!(w3cos_core::class::instance_of(&element, &constructor));
        crate::jsdom::document_value()
            .get_property("body")
            .call_method("appendChild", vec![element.clone()]);
        assert!(element.get_property("connected").to_bool());
    }

    #[test]
    fn registry_and_constructor_are_realm_owned() {
        reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_registry = custom_elements_value();
        let old_class = custom_element_registry_class();
        old_class
            .get_property("prototype")
            .set_property("realmMarker", Value::Bool(true));
        let old_constructor = Value::function(|_, _| Value::Undefined);
        old_registry.call_method("define", vec![Value::string("old-widget"), old_constructor]);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let new_registry = custom_elements_value();
        let new_class = custom_element_registry_class();
        assert!(!old_registry.strict_eq(&new_registry));
        assert!(!old_class.strict_eq(&new_class));
        assert!(
            !new_class
                .get_property("prototype")
                .get_property("realmMarker")
                .to_bool()
        );
        assert!(new_registry
            .call_method("get", vec![Value::string("old-widget")])
            .is_undefined());

        let stale_constructor = Value::function(|_, _| Value::Undefined);
        old_registry.call_method(
            "define",
            vec![Value::string("stale-widget"), stale_constructor],
        );
        assert!(new_registry
            .call_method("get", vec![Value::string("stale-widget")])
            .is_undefined());
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(old_class.get_property("prototype").is_undefined());
        assert!(old_registry.get_property("define").is_undefined());

        let fresh_constructor = Value::function(|_, _| Value::Undefined);
        new_registry.call_method(
            "define",
            vec![Value::string("fresh-widget"), fresh_constructor.clone()],
        );
        assert!(
            new_registry
                .call_method("get", vec![Value::string("fresh-widget")])
                .strict_eq(&fresh_constructor)
        );
    }

    #[test]
    fn internals_state_sets_and_pseudo_elements_are_realm_owned() {
        reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_internals_class = element_internals_class();
        let old_states_class = custom_state_set_class();
        let old_pseudo_class = css_pseudo_element_class();
        let element = crate::jsdom::document_value()
            .call_method("createElement", vec![Value::string("x-internals")]);
        let element_weak = crate::jsdom::weak_realm_object(&element);
        let internals = element.call_method("attachInternals", Vec::new());
        let states = internals.get_property("states");
        states.call_method("add", vec![Value::string("--busy")]);
        let pseudo = element.call_method("pseudo", vec![Value::string("::before")]);
        let internals_weak = crate::jsdom::weak_realm_object(&internals);
        let states_weak = crate::jsdom::weak_realm_object(&states);
        let pseudo_weak = crate::jsdom::weak_realm_object(&pseudo);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_internals_class.get_property("prototype").is_undefined());
        assert!(old_states_class.get_property("prototype").is_undefined());
        assert!(old_pseudo_class.get_property("prototype").is_undefined());
        assert!(internals.get_property("form").is_undefined());
        assert!(internals.get_property("states").is_undefined());
        assert!(
            internals
                .call_method("checkValidity", Vec::new())
                .is_undefined()
        );
        assert!(states.get_property("size").is_undefined());
        assert!(
            states
                .call_method("add", vec![Value::string("--stale")])
                .is_undefined()
        );
        assert!(pseudo.get_property("element").is_undefined());
        assert!(pseudo.get_property("parent").is_undefined());
        assert!(
            pseudo
                .call_method("pseudo", vec![Value::string("::marker")])
                .is_undefined()
        );

        drop(element);
        drop(internals);
        drop(states);
        drop(pseudo);
        assert!(element_weak.upgrade().is_none());
        assert!(internals_weak.upgrade().is_none());
        assert!(states_weak.upgrade().is_none());
        assert!(pseudo_weak.upgrade().is_none());
    }

    #[test]
    fn element_internals_manage_custom_states_validity_and_form_owner() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let document = crate::jsdom::document_value();
        let form = document.call_method("createElement", vec![Value::string("form")]);
        let control = document.call_method("createElement", vec![Value::string("x-control")]);
        document
            .get_property("body")
            .call_method("appendChild", vec![form.clone()]);
        form.call_method("appendChild", vec![control.clone()]);
        let internals = control.call_method("attachInternals", vec![]);
        assert!(w3cos_core::class::instance_of(
            &internals,
            &element_internals_class()
        ));
        assert!(internals.get_property("form") == form);
        let states = internals.get_property("states");
        states.call_method("add", vec![Value::string("--loading")]);
        assert!(w3cos_core::class::instance_of(
            &states,
            &custom_state_set_class()
        ));
        assert!(
            states
                .call_method("has", vec![Value::string("--loading")])
                .to_bool()
        );
        assert_eq!(states.get_property("size").to_number(), 1.0);

        let invalid_calls = Rc::new(std::cell::Cell::new(0));
        let invalid_calls_for_listener = Rc::clone(&invalid_calls);
        control.call_method(
            "addEventListener",
            vec![
                Value::string("invalid"),
                Value::function(move |_, _| {
                    invalid_calls_for_listener.set(invalid_calls_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        internals.call_method(
            "setValidity",
            vec![
                Value::object(HashMap::from([("customError".into(), Value::Bool(true))])),
                Value::string("not ready"),
            ],
        );
        assert!(!internals.call_method("checkValidity", vec![]).to_bool());
        assert_eq!(invalid_calls.get(), 1);
        assert_eq!(
            internals.get_property("validationMessage").to_js_string(),
            "not ready"
        );
        assert!(
            internals
                .get_property("validity")
                .get_property("customError")
                .to_bool()
        );
        internals.call_method("setValidity", vec![Value::object(HashMap::new())]);
        assert!(internals.call_method("checkValidity", vec![]).to_bool());
    }

    #[test]
    fn css_pseudo_elements_keep_origin_and_parent_chain() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let element =
            crate::jsdom::document_value().call_method("createElement", vec![Value::string("div")]);
        let before = element.call_method("pseudo", vec![Value::string("::before")]);
        assert!(w3cos_core::class::instance_of(
            &before,
            &css_pseudo_element_class()
        ));
        assert!(before.get_property("element") == element);
        assert!(before.get_property("parent") == element);
        let marker = before.call_method("pseudo", vec![Value::string("::marker")]);
        assert!(marker.get_property("parent") == before);
        assert!(marker.get_property("element") == element);
        assert_eq!(marker.get_property("type").to_js_string(), "::marker");
    }
}
