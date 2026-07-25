//! CSSOM rule constructor identities and stylesheet-backed style-rule values.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

pub const CLASS_NAMES: &[&str] = &[
    "CSSRule",
    "CSSRuleList",
    "CSSStyleRule",
    "CSSConditionRule",
    "CSSGroupingRule",
    "CSSContainerRule",
    "CSSCounterStyleRule",
    "CSSFontFaceRule",
    "CSSFontFeatureValuesRule",
    "CSSFontPaletteValuesRule",
    "CSSFunctionDeclarations",
    "CSSFunctionDescriptors",
    "CSSFunctionRule",
    "CSSImportRule",
    "CSSKeyframeRule",
    "CSSKeyframesRule",
    "CSSLayerBlockRule",
    "CSSLayerStatementRule",
    "CSSMarginRule",
    "CSSMediaRule",
    "CSSNamespaceRule",
    "CSSNestedDeclarations",
    "CSSPageRule",
    "CSSPositionTryDescriptors",
    "CSSPositionTryRule",
    "CSSPropertyRule",
    "CSSScopeRule",
    "CSSStartingStyleRule",
    "CSSSupportsRule",
    "CSSViewTransitionRule",
];

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn parent(name: &str) -> Option<&'static str> {
    match name {
        "CSSGroupingRule" => Some("CSSRule"),
        "CSSConditionRule" => Some("CSSGroupingRule"),
        "CSSContainerRule" | "CSSMediaRule" | "CSSScopeRule" | "CSSSupportsRule" => {
            Some("CSSConditionRule")
        }
        "CSSLayerBlockRule" | "CSSStartingStyleRule" => Some("CSSGroupingRule"),
        "CSSRule"
        | "CSSRuleList"
        | "CSSFunctionDeclarations"
        | "CSSFunctionDescriptors"
        | "CSSPositionTryDescriptors" => None,
        _ => Some("CSSRule"),
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
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string(&format!("Illegal constructor: {name}")),
                ),
            ])))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype.clone());
        classes.borrow_mut().insert(name.to_string(), class.clone());
        if let Some(parent) = parent(name) {
            w3cos_core::class::set_prototype_of(
                &prototype,
                &self::class(parent).get_property("prototype"),
            );
        }
        let members = match name {
            "CSSRule" => "cssText parentRule parentStyleSheet type",
            "CSSRuleList" => "item length",
            "CSSStyleRule" => "cssRules deleteRule insertRule selectorText style styleMap",
            "CSSConditionRule" => "conditionText",
            "CSSGroupingRule" => "cssRules deleteRule insertRule",
            "CSSContainerRule" => "conditions containerName containerQuery",
            "CSSCounterStyleRule" => {
                "additiveSymbols fallback name negative pad prefix range speakAs suffix symbols system"
            }
            "CSSFontFaceRule" => "style",
            "CSSFontFeatureValuesRule" => {
                "annotation characterVariant fontFamily ornaments styleset stylistic swash"
            }
            "CSSFontPaletteValuesRule" => {
                "basePalette fontFamily name overrideColors"
            }
            "CSSFunctionDeclarations" => "style",
            "CSSFunctionDescriptors" => "result",
            "CSSFunctionRule" => "getParameters name returnType",
            "CSSImportRule" => "href layerName media styleSheet supportsText",
            "CSSKeyframeRule" => "keyText style",
            "CSSKeyframesRule" => {
                "appendRule cssRules deleteRule findRule length name"
            }
            "CSSLayerBlockRule" => "name",
            "CSSLayerStatementRule" => "nameList",
            "CSSMarginRule" => "name style",
            "CSSMediaRule" => "media",
            "CSSNamespaceRule" => "namespaceURI prefix",
            "CSSNestedDeclarations" => "style",
            "CSSPageRule" => "selectorText style",
            "CSSPositionTryDescriptors" => {
                "align-self alignSelf block-size blockSize bottom height inline-size inlineSize \
                 inset inset-block inset-block-end inset-block-start inset-inline \
                 inset-inline-end inset-inline-start insetBlock insetBlockEnd insetBlockStart \
                 insetInline insetInlineEnd insetInlineStart justify-self justifySelf left margin \
                 margin-block margin-block-end margin-block-start margin-bottom margin-inline \
                 margin-inline-end margin-inline-start margin-left margin-right margin-top \
                 marginBlock marginBlockEnd marginBlockStart marginBottom marginInline \
                 marginInlineEnd marginInlineStart marginLeft marginRight marginTop max-block-size \
                 max-height max-inline-size max-width maxBlockSize maxHeight maxInlineSize \
                 maxWidth min-block-size min-height min-inline-size min-width minBlockSize \
                 minHeight minInlineSize minWidth place-self placeSelf position-anchor \
                 position-area positionAnchor positionArea right top width"
            }
            "CSSPositionTryRule" => "name style",
            "CSSPropertyRule" => "inherits initialValue name syntax",
            "CSSScopeRule" => "end start",
            "CSSViewTransitionRule" => "navigation types",
            _ => "",
        };
        for member in members.split_whitespace() {
            prototype.set_property(member, Value::Undefined);
        }
        if name == "CSSRule" {
            for (constant, value) in [
                ("STYLE_RULE", 1.0),
                ("CHARSET_RULE", 2.0),
                ("IMPORT_RULE", 3.0),
                ("MEDIA_RULE", 4.0),
                ("FONT_FACE_RULE", 5.0),
                ("PAGE_RULE", 6.0),
                ("KEYFRAMES_RULE", 7.0),
                ("KEYFRAME_RULE", 8.0),
                ("MARGIN_RULE", 9.0),
                ("NAMESPACE_RULE", 10.0),
                ("COUNTER_STYLE_RULE", 11.0),
                ("SUPPORTS_RULE", 12.0),
                ("FONT_FEATURE_VALUES_RULE", 14.0),
            ] {
                class.set_property(constant, Value::Number(value));
                prototype.set_property(constant, Value::Number(value));
            }
        }
        class
    })
}

pub fn style_rule_value(css_text: &str, selector: &str) -> Value {
    let declaration = css_text
        .split_once('{')
        .and_then(|(_, body)| body.rsplit_once('}').map(|(body, _)| body.trim()))
        .unwrap_or_default();
    let value = Value::object(HashMap::from([
        ("cssText".into(), Value::string(css_text)),
        ("selectorText".into(), Value::string(selector)),
        ("type".into(), Value::Number(1.0)),
        ("parentRule".into(), Value::Null),
        ("parentStyleSheet".into(), Value::Null),
        (
            "style".into(),
            Value::object(HashMap::from([(
                "cssText".into(),
                Value::string(declaration),
            )])),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &class("CSSStyleRule").get_property("prototype"));
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_rule_has_standard_identity_and_fields() {
        let value = style_rule_value("main { color: red; }", "main");
        assert!(w3cos_core::class::instance_of(
            &value,
            &class("CSSStyleRule")
        ));
        assert_eq!(value.get_property("selectorText").to_js_string(), "main");
        assert_eq!(
            value
                .get_property("style")
                .get_property("cssText")
                .to_js_string(),
            "color: red;"
        );
    }
}
