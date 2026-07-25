//! Legacy SVG value-object identities.
//!
//! Chromium still exposes these interfaces even though author code normally
//! receives their instances from animated SVG attributes. The compact DOM
//! bridge exposes their constructor/prototype identities and constants while
//! attribute-backed live object creation is implemented incrementally.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

pub const SVG_VALUE_CLASS_NAMES: &[&str] = &[
    "SVGAngle",
    "SVGAnimatedAngle",
    "SVGAnimatedBoolean",
    "SVGAnimatedEnumeration",
    "SVGAnimatedInteger",
    "SVGAnimatedLength",
    "SVGAnimatedLengthList",
    "SVGAnimatedNumber",
    "SVGAnimatedNumberList",
    "SVGAnimatedPreserveAspectRatio",
    "SVGAnimatedRect",
    "SVGAnimatedString",
    "SVGAnimatedTransformList",
    "SVGLength",
    "SVGLengthList",
    "SVGNumber",
    "SVGNumberList",
    "SVGPointList",
    "SVGPreserveAspectRatio",
    "SVGStringList",
    "SVGTransform",
    "SVGTransformList",
    "SVGUnitTypes",
];

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn install_surface(name: &str, class: &Value) {
    let members = match name {
        "SVGAngle" => {
            "convertToSpecifiedUnits newValueSpecifiedUnits unitType value valueAsString \
             valueInSpecifiedUnits"
        }
        "SVGAnimatedAngle"
        | "SVGAnimatedBoolean"
        | "SVGAnimatedEnumeration"
        | "SVGAnimatedInteger"
        | "SVGAnimatedLength"
        | "SVGAnimatedLengthList"
        | "SVGAnimatedNumber"
        | "SVGAnimatedNumberList"
        | "SVGAnimatedPreserveAspectRatio"
        | "SVGAnimatedRect"
        | "SVGAnimatedString"
        | "SVGAnimatedTransformList" => "animVal baseVal",
        "SVGLength" => {
            "convertToSpecifiedUnits newValueSpecifiedUnits unitType value valueAsString \
             valueInSpecifiedUnits"
        }
        "SVGLengthList" | "SVGNumberList" | "SVGPointList" | "SVGStringList" => {
            "appendItem clear getItem initialize insertItemBefore length numberOfItems \
             removeItem replaceItem"
        }
        "SVGNumber" => "value",
        "SVGPreserveAspectRatio" => "align meetOrSlice",
        "SVGTransform" => {
            "angle matrix setMatrix setRotate setScale setSkewX setSkewY setTranslate type"
        }
        "SVGTransformList" => {
            "appendItem clear consolidate createSVGTransformFromMatrix getItem initialize \
             insertItemBefore length numberOfItems removeItem replaceItem"
        }
        _ => "",
    };
    let prototype = class.get_property("prototype");
    for member in members.split_whitespace() {
        prototype.set_property(member, Value::Undefined);
    }

    let constants = match name {
        "SVGAngle" => {
            "SVG_ANGLETYPE_UNKNOWN:0 SVG_ANGLETYPE_UNSPECIFIED:1 SVG_ANGLETYPE_DEG:2 \
             SVG_ANGLETYPE_RAD:3 SVG_ANGLETYPE_GRAD:4"
        }
        "SVGLength" => {
            "SVG_LENGTHTYPE_UNKNOWN:0 SVG_LENGTHTYPE_NUMBER:1 SVG_LENGTHTYPE_PERCENTAGE:2 \
             SVG_LENGTHTYPE_EMS:3 SVG_LENGTHTYPE_EXS:4 SVG_LENGTHTYPE_PX:5 \
             SVG_LENGTHTYPE_CM:6 SVG_LENGTHTYPE_MM:7 SVG_LENGTHTYPE_IN:8 \
             SVG_LENGTHTYPE_PT:9 SVG_LENGTHTYPE_PC:10"
        }
        "SVGPreserveAspectRatio" => {
            "SVG_PRESERVEASPECTRATIO_UNKNOWN:0 SVG_PRESERVEASPECTRATIO_NONE:1 \
             SVG_PRESERVEASPECTRATIO_XMINYMIN:2 SVG_PRESERVEASPECTRATIO_XMIDYMIN:3 \
             SVG_PRESERVEASPECTRATIO_XMAXYMIN:4 SVG_PRESERVEASPECTRATIO_XMINYMID:5 \
             SVG_PRESERVEASPECTRATIO_XMIDYMID:6 SVG_PRESERVEASPECTRATIO_XMAXYMID:7 \
             SVG_PRESERVEASPECTRATIO_XMINYMAX:8 SVG_PRESERVEASPECTRATIO_XMIDYMAX:9 \
             SVG_PRESERVEASPECTRATIO_XMAXYMAX:10 SVG_MEETORSLICE_UNKNOWN:0 \
             SVG_MEETORSLICE_MEET:1 SVG_MEETORSLICE_SLICE:2"
        }
        "SVGTransform" => {
            "SVG_TRANSFORM_UNKNOWN:0 SVG_TRANSFORM_MATRIX:1 SVG_TRANSFORM_TRANSLATE:2 \
             SVG_TRANSFORM_SCALE:3 SVG_TRANSFORM_ROTATE:4 SVG_TRANSFORM_SKEWX:5 \
             SVG_TRANSFORM_SKEWY:6"
        }
        "SVGUnitTypes" => {
            "SVG_UNIT_TYPE_UNKNOWN:0 SVG_UNIT_TYPE_USERSPACEONUSE:1 \
             SVG_UNIT_TYPE_OBJECTBOUNDINGBOX:2"
        }
        _ => "",
    };
    for constant in constants.split_whitespace() {
        let (constant, value) = constant
            .split_once(':')
            .expect("SVG value constant manifest must include a value");
        let value = Value::Number(
            value
                .parse::<f64>()
                .expect("SVG value constant must be numeric"),
        );
        class.set_property(constant, value.clone());
        prototype.set_property(constant, value);
    }
}

pub fn class(name: &str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let name_for_error = name.to_string();
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string(&format!("Illegal constructor: {name_for_error}")),
                ),
            ])))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        install_surface(name, &class);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

pub fn geometry_alias_class(name: &str) -> Value {
    let (geometry, members) = match name {
        "SVGPoint" => ("DOMPoint", "matrixTransform"),
        "SVGRect" => ("DOMRect", ""),
        "SVGMatrix" => (
            "DOMMatrix",
            "flipX flipY inverse multiply rotate rotateFromVector scale scaleNonUniform \
             skewX skewY translate",
        ),
        _ => return Value::Undefined,
    };
    let class = crate::geometry_web::class(geometry);
    let prototype = class.get_property("prototype");
    for member in members.split_whitespace() {
        prototype.set_property(member, Value::Undefined);
    }
    class
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_value_classes_have_stable_illegal_constructor_identity() {
        let first = class("SVGLength");
        let second = class("SVGLength");
        assert!(first.strict_eq(&second));
        assert_eq!(first.get_property("name").to_js_string(), "SVGLength");
        assert!(first.get_property("prototype").is_object());
    }
}
