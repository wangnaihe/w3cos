//! Deterministic data-oriented CSS Typed OM values.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

pub const CLASS_NAMES: &[&str] = &[
    "CSSStyleValue",
    "CSSKeywordValue",
    "CSSNumericValue",
    "CSSUnitValue",
    "CSSMathValue",
    "CSSMathSum",
    "CSSMathProduct",
    "CSSMathNegate",
    "CSSMathInvert",
    "CSSMathMin",
    "CSSMathMax",
    "CSSMathClamp",
    "CSSNumericArray",
    "CSSUnparsedValue",
    "CSSVariableReferenceValue",
    "CSSImageValue",
    "CSSPositionValue",
    "CSSTransformComponent",
    "CSSMatrixComponent",
    "CSSPerspective",
    "CSSRotate",
    "CSSScale",
    "CSSSkew",
    "CSSSkewX",
    "CSSSkewY",
    "CSSTranslate",
    "CSSTransformValue",
    "StylePropertyMapReadOnly",
    "StylePropertyMap",
];

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn type_error(message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ]))
}

fn set_identity(value: &Value, name: &str) {
    w3cos_core::class::set_prototype_of(value, &class(name).get_property("prototype"));
}

pub(crate) fn serialize_value(value: &Value) -> String {
    let method = value.get_property("toString");
    if method.is_function() {
        method.call(value.clone(), vec![]).to_js_string()
    } else {
        value.to_js_string()
    }
}

fn keyword_value(input: Value) -> Value {
    let value = Value::object(HashMap::from([(
        "value".into(),
        Value::string(&input.to_js_string()),
    )]));
    value.set_property(
        "toString",
        Value::function(|this, _| Value::string(&this.get_property("value").to_js_string())),
    );
    set_identity(&value, "CSSKeywordValue");
    value
}

fn unit_value(number: f64, unit: String) -> Value {
    if !number.is_finite() || unit.trim().is_empty() {
        w3cos_core::throw_value(type_error(
            "CSSUnitValue requires a finite value and non-empty unit",
        ));
    }
    let value = Value::object(HashMap::from([
        ("value".into(), Value::Number(number)),
        ("unit".into(), Value::string(&unit)),
    ]));
    value.set_property(
        "toString",
        Value::function(|this, _| {
            let number = this.get_property("value").to_number();
            let number = if number.fract() == 0.0 {
                format!("{}", number as i64)
            } else {
                number.to_string()
            };
            Value::string(&format!(
                "{number}{}",
                this.get_property("unit").to_js_string()
            ))
        }),
    );
    value.set_property(
        "to",
        Value::function(|this, args| {
            let unit = args.first().cloned().unwrap_or_default().to_js_string();
            if unit != this.get_property("unit").to_js_string() {
                w3cos_core::throw_value(type_error(
                    "cross-unit CSSUnitValue conversion is not available",
                ));
            }
            unit_value(this.get_property("value").to_number(), unit)
        }),
    );
    set_identity(&value, "CSSUnitValue");
    value
}

pub(crate) fn parse_style_value(text: &str) -> Value {
    let split = text
        .char_indices()
        .find(|(_, character)| {
            !character.is_ascii_digit() && !matches!(character, '+' | '-' | '.' | 'e' | 'E')
        })
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    if !number.is_empty()
        && !unit.is_empty()
        && let Ok(number) = number.parse::<f64>()
    {
        return unit_value(number, unit.to_string());
    }
    keyword_value(Value::string(text))
}

fn numeric_array(values: Vec<Value>) -> Value {
    let array = Value::array(values);
    set_identity(&array, "CSSNumericArray");
    array
}

fn math_value(name: &'static str, input: Vec<Value>) -> Value {
    let values = numeric_array(input);
    let value = Value::object(HashMap::from([("values".into(), values.clone())]));
    if matches!(name, "CSSMathNegate" | "CSSMathInvert") {
        value.set_property("value", values.get_property("0"));
    } else if name == "CSSMathClamp" {
        value.set_property("lower", values.get_property("0"));
        value.set_property("value", values.get_property("1"));
        value.set_property("upper", values.get_property("2"));
    }
    value.set_property(
        "operator",
        Value::string(match name {
            "CSSMathSum" => "sum",
            "CSSMathProduct" => "product",
            "CSSMathNegate" => "negate",
            "CSSMathInvert" => "invert",
            "CSSMathMin" => "min",
            "CSSMathMax" => "max",
            "CSSMathClamp" => "clamp",
            _ => "sum",
        }),
    );
    value.set_property(
        "toString",
        Value::function(move |_, _| {
            let parts = values
                .iter()
                .map(|part| serialize_value(&part))
                .collect::<Vec<_>>();
            let text = match name {
                "CSSMathSum" => format!("calc({})", parts.join(" + ")),
                "CSSMathProduct" => format!("calc({})", parts.join(" * ")),
                "CSSMathNegate" => {
                    format!("calc(-({}))", parts.first().cloned().unwrap_or_default())
                }
                "CSSMathInvert" => {
                    format!("calc(1 / ({}))", parts.first().cloned().unwrap_or_default())
                }
                "CSSMathMin" => format!("min({})", parts.join(", ")),
                "CSSMathMax" => format!("max({})", parts.join(", ")),
                "CSSMathClamp" => format!("clamp({})", parts.join(", ")),
                _ => unreachable!(),
            };
            Value::string(&text)
        }),
    );
    set_identity(&value, name);
    value
}

fn unparsed_value(input: Value) -> Value {
    let value = Value::array(input.iter().collect());
    value.set_property(
        "toString",
        Value::function(|this, _| {
            Value::string(
                &this
                    .iter()
                    .map(|part| serialize_value(&part))
                    .collect::<Vec<_>>()
                    .join(""),
            )
        }),
    );
    set_identity(&value, "CSSUnparsedValue");
    value
}

fn transform_component(name: &'static str, args: Vec<Value>) -> Value {
    let value = Value::object(HashMap::new());
    match name {
        "CSSMatrixComponent" => value.set_property(
            "matrix",
            args.first().cloned().unwrap_or_else(|| {
                w3cos_core::class::construct(&crate::geometry_web::class("DOMMatrix"), vec![])
            }),
        ),
        "CSSPerspective" => {
            value.set_property("length", args.first().cloned().unwrap_or(Value::Undefined))
        }
        "CSSRotate" => {
            value.set_property("angle", args.first().cloned().unwrap_or(Value::Undefined));
            value.set_property("x", Value::Number(0.0));
            value.set_property("y", Value::Number(0.0));
            value.set_property("z", Value::Number(1.0));
        }
        "CSSScale" => {
            value.set_property("x", args.first().cloned().unwrap_or(Value::Number(1.0)));
            value.set_property("y", args.get(1).cloned().unwrap_or(Value::Number(1.0)));
            value.set_property("z", args.get(2).cloned().unwrap_or(Value::Number(1.0)));
        }
        "CSSSkew" => {
            value.set_property("ax", args.first().cloned().unwrap_or(Value::Undefined));
            value.set_property("ay", args.get(1).cloned().unwrap_or(Value::Undefined));
        }
        "CSSSkewX" => {
            value.set_property("ax", args.first().cloned().unwrap_or(Value::Undefined));
        }
        "CSSSkewY" => {
            value.set_property("ay", args.first().cloned().unwrap_or(Value::Undefined));
        }
        "CSSTranslate" => {
            value.set_property("x", args.first().cloned().unwrap_or(Value::Undefined));
            value.set_property("y", args.get(1).cloned().unwrap_or(Value::Undefined));
            value.set_property("z", args.get(2).cloned().unwrap_or(Value::Number(0.0)));
        }
        _ => {}
    }
    value.set_property("is2D", Value::Bool(true));
    value.set_property(
        "toMatrix",
        Value::function(|_, _| {
            w3cos_core::class::construct(&crate::geometry_web::class("DOMMatrix"), vec![])
        }),
    );
    value.set_property(
        "toString",
        Value::function(move |this, _| {
            let text = match name {
                "CSSMatrixComponent" => this.get_property("matrix").to_js_string(),
                "CSSPerspective" => {
                    format!(
                        "perspective({})",
                        serialize_value(&this.get_property("length"))
                    )
                }
                "CSSRotate" => {
                    format!("rotate({})", serialize_value(&this.get_property("angle")))
                }
                "CSSScale" => format!(
                    "scale({}, {})",
                    serialize_value(&this.get_property("x")),
                    serialize_value(&this.get_property("y"))
                ),
                "CSSSkew" => format!(
                    "skew({}, {})",
                    serialize_value(&this.get_property("ax")),
                    serialize_value(&this.get_property("ay"))
                ),
                "CSSSkewX" => {
                    format!("skewX({})", serialize_value(&this.get_property("ax")))
                }
                "CSSSkewY" => {
                    format!("skewY({})", serialize_value(&this.get_property("ay")))
                }
                "CSSTranslate" => format!(
                    "translate({}, {})",
                    serialize_value(&this.get_property("x")),
                    serialize_value(&this.get_property("y"))
                ),
                _ => String::new(),
            };
            Value::string(&text)
        }),
    );
    set_identity(&value, name);
    value
}

fn transform_value(input: Value) -> Value {
    let components = Rc::new(input.iter().collect::<Vec<_>>());
    let mut properties = HashMap::from([
        ("length".into(), Value::Number(components.len() as f64)),
        ("is2D".into(), Value::Bool(true)),
    ]);
    for (index, component) in components.iter().enumerate() {
        properties.insert(index.to_string(), component.clone());
    }
    let value = Value::object(properties);
    let entries = Rc::clone(&components);
    value.set_property(
        "entries",
        Value::function(move |_, _| {
            Value::array(
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        Value::array(vec![Value::Number(index as f64), value.clone()])
                    })
                    .collect(),
            )
            .call_method("__w3cos_symbol_iterator", vec![])
        }),
    );
    let keys = Rc::clone(&components);
    value.set_property(
        "keys",
        Value::function(move |_, _| {
            Value::array(
                (0..keys.len())
                    .map(|index| Value::Number(index as f64))
                    .collect(),
            )
            .call_method("__w3cos_symbol_iterator", vec![])
        }),
    );
    let values = Rc::clone(&components);
    let values_method = Value::function(move |_, _| {
        Value::array(values.as_ref().clone()).call_method("__w3cos_symbol_iterator", vec![])
    });
    value.set_property("values", values_method.clone());
    value.set_property("__w3cos_symbol_iterator", values_method);
    let each = Rc::clone(&components);
    let each_value = value.clone();
    value.set_property(
        "forEach",
        Value::function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                w3cos_core::throw_value(type_error(
                    "CSSTransformValue.forEach requires a callback",
                ));
            }
            for (index, component) in each.iter().enumerate() {
                callback.call(
                    this_arg.clone(),
                    vec![
                        component.clone(),
                        Value::Number(index as f64),
                        each_value.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "toMatrix",
        Value::function(|_, _| {
            w3cos_core::class::construct(&crate::geometry_web::class("DOMMatrix"), vec![])
        }),
    );
    let components_for_string = Rc::clone(&components);
    value.set_property(
        "toString",
        Value::function(move |_, _| {
            Value::string(
                &components_for_string
                    .iter()
                    .map(serialize_value)
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }),
    );
    set_identity(&value, "CSSTransformValue");
    value
}

fn build_class(name: &'static str) -> Value {
    let class = match name {
        "CSSKeywordValue" => Value::function(|_, args| {
            keyword_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        "CSSUnitValue" => Value::function(|_, args| {
            unit_value(
                args.first().map(Value::to_number).unwrap_or(f64::NAN),
                args.get(1).cloned().unwrap_or_default().to_js_string(),
            )
        }),
        "CSSMathSum" | "CSSMathProduct" | "CSSMathMin" | "CSSMathMax" | "CSSMathClamp" => {
            Value::function(move |_, args| math_value(name, args))
        }
        "CSSMathNegate" | "CSSMathInvert" => Value::function(move |_, args| {
            math_value(
                name,
                vec![args.first().cloned().unwrap_or(Value::Undefined)],
            )
        }),
        "CSSUnparsedValue" => Value::function(|_, args| {
            unparsed_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        "CSSMatrixComponent" | "CSSPerspective" | "CSSRotate" | "CSSScale" | "CSSSkew"
        | "CSSSkewX" | "CSSSkewY" | "CSSTranslate" => {
            Value::function(move |_, args| transform_component(name, args))
        }
        "CSSTransformValue" => Value::function(|_, args| {
            transform_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        "CSSPositionValue" => Value::function(|_, args| {
            let value = Value::object(HashMap::from([
                (
                    "x".into(),
                    args.first().cloned().unwrap_or(Value::Undefined),
                ),
                ("y".into(), args.get(1).cloned().unwrap_or(Value::Undefined)),
            ]));
            value.set_property(
                "toString",
                Value::function(|this, _| {
                    Value::string(&format!(
                        "{} {}",
                        serialize_value(&this.get_property("x")),
                        serialize_value(&this.get_property("y"))
                    ))
                }),
            );
            set_identity(&value, "CSSPositionValue");
            value
        }),
        "CSSVariableReferenceValue" => Value::function(|_, args| {
            let variable = args.first().cloned().unwrap_or_default().to_js_string();
            if !variable.starts_with("--") {
                w3cos_core::throw_value(type_error("CSS variable names must start with --"));
            }
            let value = Value::object(HashMap::from([
                ("variable".into(), Value::string(&variable)),
                (
                    "fallback".into(),
                    args.get(1).cloned().unwrap_or(Value::Null),
                ),
            ]));
            value.set_property(
                "toString",
                Value::function(|this, _| {
                    let variable = this.get_property("variable").to_js_string();
                    let fallback = this.get_property("fallback");
                    if fallback.is_null() || fallback.is_undefined() {
                        Value::string(&format!("var({variable})"))
                    } else {
                        Value::string(&format!("var({variable}, {})", serialize_value(&fallback)))
                    }
                }),
            );
            set_identity(&value, "CSSVariableReferenceValue");
            value
        }),
        _ => Value::function(move |_, _| {
            w3cos_core::throw_value(type_error(&format!("Illegal constructor: {name}")))
        }),
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    class.set_property("prototype", prototype);
    class
}

fn install(name: &'static str, class_value: &Value) {
    let prototype = class_value.get_property("prototype");
    let parent = match name {
        "CSSKeywordValue"
        | "CSSNumericValue"
        | "CSSUnparsedValue"
        | "CSSVariableReferenceValue" => Some("CSSStyleValue"),
        "CSSUnitValue" | "CSSMathValue" => Some("CSSNumericValue"),
        "CSSMathSum" | "CSSMathProduct" | "CSSMathNegate" | "CSSMathInvert" | "CSSMathMin"
        | "CSSMathMax" | "CSSMathClamp" => Some("CSSMathValue"),
        "CSSPositionValue" | "CSSTransformValue" | "CSSImageValue" => Some("CSSStyleValue"),
        "CSSMatrixComponent" | "CSSPerspective" | "CSSRotate" | "CSSScale" | "CSSSkew"
        | "CSSSkewX" | "CSSSkewY" | "CSSTranslate" => Some("CSSTransformComponent"),
        "StylePropertyMap" => Some("StylePropertyMapReadOnly"),
        _ => None,
    };
    if let Some(parent) = parent {
        w3cos_core::class::set_prototype_of(&prototype, &class(parent).get_property("prototype"));
    }
    if name == "CSSStyleValue" {
        prototype.set_property("toString", Value::Undefined);
        for method in ["parse", "parseAll"] {
            class_value.set_property(
                method,
                Value::function(move |_, args| {
                    let parsed =
                        parse_style_value(&args.get(1).cloned().unwrap_or_default().to_js_string());
                    if method == "parseAll" {
                        Value::array(vec![parsed])
                    } else {
                        parsed
                    }
                }),
            );
        }
    }
    if name == "CSSNumericValue" {
        class_value.set_property(
            "parse",
            Value::function(|_, args| {
                parse_style_value(&args.first().cloned().unwrap_or_default().to_js_string())
            }),
        );
        for (member, result_class) in [
            ("add", "CSSMathSum"),
            ("mul", "CSSMathProduct"),
            ("min", "CSSMathMin"),
            ("max", "CSSMathMax"),
        ] {
            prototype.set_property(
                member,
                Value::function(move |this, args| {
                    let mut values = vec![this];
                    values.extend(args);
                    math_value(result_class, values)
                }),
            );
        }
        prototype.set_property(
            "sub",
            Value::function(|this, args| {
                let mut values = vec![this];
                values.extend(
                    args.into_iter()
                        .map(|value| math_value("CSSMathNegate", vec![value])),
                );
                math_value("CSSMathSum", values)
            }),
        );
        prototype.set_property(
            "div",
            Value::function(|this, args| {
                let mut values = vec![this];
                values.extend(
                    args.into_iter()
                        .map(|value| math_value("CSSMathInvert", vec![value])),
                );
                math_value("CSSMathProduct", values)
            }),
        );
        prototype.set_property(
            "equals",
            Value::function(|this, args| {
                Value::Bool(
                    args.iter()
                        .all(|value| serialize_value(value) == serialize_value(&this)),
                )
            }),
        );
        prototype.set_property(
            "to",
            Value::function(|_, _| {
                w3cos_core::throw_value(type_error(
                    "conversion of compound CSS numeric values is not available",
                ))
            }),
        );
        prototype.set_property(
            "toSum",
            Value::function(|this, _| math_value("CSSMathSum", vec![this])),
        );
        prototype.set_property(
            "type",
            Value::function(|_, _| Value::object(HashMap::new())),
        );
    }
    let members = match name {
        "CSSKeywordValue" => "value",
        "CSSUnitValue" => "unit value",
        "CSSMathValue" => "operator",
        "CSSMathSum" | "CSSMathProduct" | "CSSMathMin" | "CSSMathMax" => "values",
        "CSSMathNegate" | "CSSMathInvert" => "value",
        "CSSMathClamp" => "lower upper value",
        "CSSNumericArray" | "CSSUnparsedValue" => "entries forEach keys length values",
        "CSSVariableReferenceValue" => "fallback variable",
        "CSSPositionValue" => "x y",
        "CSSTransformComponent" => "is2D toMatrix toString",
        "CSSMatrixComponent" => "matrix",
        "CSSPerspective" => "length",
        "CSSRotate" => "angle x y z",
        "CSSScale" => "x y z",
        "CSSSkew" => "ax ay",
        "CSSSkewX" => "ax",
        "CSSSkewY" => "ay",
        "CSSTranslate" => "x y z",
        "CSSTransformValue" => "entries forEach is2D keys length toMatrix values",
        "StylePropertyMapReadOnly" => "entries forEach get getAll has keys size values",
        "StylePropertyMap" => "append clear delete set",
        _ => "",
    };
    for member in members.split_whitespace() {
        prototype.set_property(member, Value::Undefined);
    }
}

pub fn class(name: &str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let Some(name) = CLASS_NAMES
            .iter()
            .copied()
            .find(|candidate| candidate == &name)
        else {
            return Value::Undefined;
        };
        let class_value = build_class(name);
        classes
            .borrow_mut()
            .insert(name.to_string(), class_value.clone());
        install(name, &class_value);
        class_value
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_values_parse_and_serialize() {
        let px = w3cos_core::class::construct(
            &class("CSSUnitValue"),
            vec![Value::Number(12.0), Value::string("px")],
        );
        assert_eq!(serialize_value(&px), "12px");
        let sum = w3cos_core::class::construct(
            &class("CSSMathSum"),
            vec![px, unit_value(3.0, "px".into())],
        );
        assert_eq!(serialize_value(&sum), "calc(12px + 3px)");
        assert_eq!(
            serialize_value(
                &unit_value(2.0, "px".into())
                    .call_method("mul", vec![unit_value(4.0, "px".into())],)
            ),
            "calc(2px * 4px)"
        );
        let parsed = class("CSSNumericValue").call_method("parse", vec![Value::string("2rem")]);
        assert_eq!(serialize_value(&parsed), "2rem");
    }
}
