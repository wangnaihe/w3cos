//! Feature-neutral policy shared by every inert HTML fragment entry point.

pub(crate) fn is_active_fragment_element(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "iframe" | "object" | "embed" | "link" | "meta" | "base"
    )
}

pub(crate) fn consumes_content_when_filtered(name: &str) -> bool {
    matches!(name, "script" | "style" | "iframe" | "object")
}

pub(crate) fn is_html_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

pub(crate) fn find_raw_text_end(lower: &str, tag: &str) -> Option<usize> {
    let prefix = format!("</{tag}");
    let mut offset = 0;
    while let Some(relative) = lower[offset..].find(&prefix) {
        let start = offset + relative;
        let suffix = &lower[start + prefix.len()..];
        if suffix.chars().next().is_none_or(|character| {
            character == '>' || character == '/' || character.is_whitespace()
        }) {
            return Some(start);
        }
        offset = start + prefix.len();
    }
    None
}

pub(crate) fn is_unsafe_fragment_attribute(name: &str, value: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("on")
        || (matches!(
            name.as_str(),
            "href" | "src" | "action" | "formaction" | "xlink:href"
        ) && value
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("javascript:"))
}

pub(crate) fn is_head_element(name: &str) -> bool {
    matches!(
        name,
        "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "noscript"
            | "script"
            | "style"
            | "template"
            | "title"
    )
}

pub(crate) fn is_foreign_html_breakout(name: &str, attributes: &[(String, String)]) -> bool {
    matches!(
        name,
        "b" | "big"
            | "blockquote"
            | "body"
            | "br"
            | "center"
            | "code"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "em"
            | "embed"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "hr"
            | "i"
            | "img"
            | "li"
            | "listing"
            | "menu"
            | "meta"
            | "nobr"
            | "ol"
            | "p"
            | "pre"
            | "ruby"
            | "s"
            | "small"
            | "span"
            | "strong"
            | "strike"
            | "sub"
            | "sup"
            | "table"
            | "tt"
            | "u"
            | "ul"
            | "var"
    ) || (name == "font"
        && attributes
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "color" | "face" | "size")))
}

pub(crate) fn is_formatting_element(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "nobr"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
    )
}

pub(crate) fn adjust_foreign_tag_name<'a>(namespace: &str, name: &'a str) -> &'a str {
    if namespace != crate::html_parser_state::SVG_NAMESPACE {
        return name;
    }
    match name {
        "altglyph" => "altGlyph",
        "altglyphdef" => "altGlyphDef",
        "altglyphitem" => "altGlyphItem",
        "animatecolor" => "animateColor",
        "animatemotion" => "animateMotion",
        "animatetransform" => "animateTransform",
        "clippath" => "clipPath",
        "feblend" => "feBlend",
        "fecolormatrix" => "feColorMatrix",
        "fecomponenttransfer" => "feComponentTransfer",
        "fecomposite" => "feComposite",
        "feconvolvematrix" => "feConvolveMatrix",
        "fediffuselighting" => "feDiffuseLighting",
        "fedisplacementmap" => "feDisplacementMap",
        "fedistantlight" => "feDistantLight",
        "fedropshadow" => "feDropShadow",
        "feflood" => "feFlood",
        "fefunca" => "feFuncA",
        "fefuncb" => "feFuncB",
        "fefuncg" => "feFuncG",
        "fefuncr" => "feFuncR",
        "fegaussianblur" => "feGaussianBlur",
        "feimage" => "feImage",
        "femerge" => "feMerge",
        "femergenode" => "feMergeNode",
        "femorphology" => "feMorphology",
        "feoffset" => "feOffset",
        "fepointlight" => "fePointLight",
        "fespecularlighting" => "feSpecularLighting",
        "fespotlight" => "feSpotLight",
        "fetile" => "feTile",
        "feturbulence" => "feTurbulence",
        "foreignobject" => "foreignObject",
        "glyphref" => "glyphRef",
        "lineargradient" => "linearGradient",
        "radialgradient" => "radialGradient",
        "textpath" => "textPath",
        _ => name,
    }
}

pub(crate) fn adjust_foreign_attribute_name<'a>(namespace: &str, name: &'a str) -> &'a str {
    if namespace == crate::html_parser_state::MATHML_NAMESPACE && name == "definitionurl" {
        return "definitionURL";
    }
    if namespace != crate::html_parser_state::SVG_NAMESPACE {
        return name;
    }
    match name {
        "attributename" => "attributeName",
        "attributetype" => "attributeType",
        "basefrequency" => "baseFrequency",
        "baseprofile" => "baseProfile",
        "calcmode" => "calcMode",
        "clippathunits" => "clipPathUnits",
        "diffuseconstant" => "diffuseConstant",
        "edgemode" => "edgeMode",
        "filterunits" => "filterUnits",
        "glyphref" => "glyphRef",
        "gradienttransform" => "gradientTransform",
        "gradientunits" => "gradientUnits",
        "kernelmatrix" => "kernelMatrix",
        "kernelunitlength" => "kernelUnitLength",
        "keypoints" => "keyPoints",
        "keysplines" => "keySplines",
        "keytimes" => "keyTimes",
        "lengthadjust" => "lengthAdjust",
        "limitingconeangle" => "limitingConeAngle",
        "markerheight" => "markerHeight",
        "markerunits" => "markerUnits",
        "markerwidth" => "markerWidth",
        "maskcontentunits" => "maskContentUnits",
        "maskunits" => "maskUnits",
        "numoctaves" => "numOctaves",
        "pathlength" => "pathLength",
        "patterncontentunits" => "patternContentUnits",
        "patterntransform" => "patternTransform",
        "patternunits" => "patternUnits",
        "pointsatx" => "pointsAtX",
        "pointsaty" => "pointsAtY",
        "pointsatz" => "pointsAtZ",
        "preservealpha" => "preserveAlpha",
        "preserveaspectratio" => "preserveAspectRatio",
        "primitiveunits" => "primitiveUnits",
        "refx" => "refX",
        "refy" => "refY",
        "repeatcount" => "repeatCount",
        "repeatdur" => "repeatDur",
        "requiredextensions" => "requiredExtensions",
        "requiredfeatures" => "requiredFeatures",
        "specularconstant" => "specularConstant",
        "specularexponent" => "specularExponent",
        "spreadmethod" => "spreadMethod",
        "startoffset" => "startOffset",
        "stddeviation" => "stdDeviation",
        "stitchtiles" => "stitchTiles",
        "surfacescale" => "surfaceScale",
        "systemlanguage" => "systemLanguage",
        "tablevalues" => "tableValues",
        "targetx" => "targetX",
        "targety" => "targetY",
        "textlength" => "textLength",
        "viewbox" => "viewBox",
        "viewtarget" => "viewTarget",
        "xchannelselector" => "xChannelSelector",
        "ychannelselector" => "yChannelSelector",
        "zoomandpan" => "zoomAndPan",
        _ => name,
    }
}

pub(crate) struct AdjustedForeignAttribute<'a> {
    pub(crate) qualified_name: &'a str,
    pub(crate) namespace: Option<&'static str>,
    pub(crate) prefix: Option<&'static str>,
    pub(crate) local_name: &'a str,
}

pub(crate) fn adjust_foreign_attribute<'a>(
    element_namespace: &str,
    name: &'a str,
) -> AdjustedForeignAttribute<'a> {
    use crate::html_parser_state::{
        HTML_NAMESPACE, XLINK_NAMESPACE, XML_NAMESPACE, XMLNS_NAMESPACE,
    };

    let qualified_name = adjust_foreign_attribute_name(element_namespace, name);
    if element_namespace == HTML_NAMESPACE {
        return AdjustedForeignAttribute {
            qualified_name,
            namespace: None,
            prefix: None,
            local_name: qualified_name,
        };
    }
    match name {
        "xlink:actuate" => adjusted_namespaced(qualified_name, XLINK_NAMESPACE, "xlink", "actuate"),
        "xlink:arcrole" => adjusted_namespaced(qualified_name, XLINK_NAMESPACE, "xlink", "arcrole"),
        "xlink:href" => adjusted_namespaced(qualified_name, XLINK_NAMESPACE, "xlink", "href"),
        "xlink:role" => adjusted_namespaced(qualified_name, XLINK_NAMESPACE, "xlink", "role"),
        "xlink:show" => adjusted_namespaced(qualified_name, XLINK_NAMESPACE, "xlink", "show"),
        "xlink:title" => adjusted_namespaced(qualified_name, XLINK_NAMESPACE, "xlink", "title"),
        "xlink:type" => adjusted_namespaced(qualified_name, XLINK_NAMESPACE, "xlink", "type"),
        "xml:base" => adjusted_namespaced(qualified_name, XML_NAMESPACE, "xml", "base"),
        "xml:lang" => adjusted_namespaced(qualified_name, XML_NAMESPACE, "xml", "lang"),
        "xml:space" => adjusted_namespaced(qualified_name, XML_NAMESPACE, "xml", "space"),
        "xmlns" => AdjustedForeignAttribute {
            qualified_name,
            namespace: Some(XMLNS_NAMESPACE),
            prefix: None,
            local_name: "xmlns",
        },
        "xmlns:xlink" => adjusted_namespaced(qualified_name, XMLNS_NAMESPACE, "xmlns", "xlink"),
        _ => AdjustedForeignAttribute {
            qualified_name,
            namespace: None,
            prefix: None,
            local_name: qualified_name,
        },
    }
}

fn adjusted_namespaced<'a>(
    qualified_name: &'a str,
    namespace: &'static str,
    prefix: &'static str,
    local_name: &'a str,
) -> AdjustedForeignAttribute<'a> {
    AdjustedForeignAttribute {
        qualified_name,
        namespace: Some(namespace),
        prefix: Some(prefix),
        local_name,
    }
}

pub(crate) fn is_special_html_element(node: u32) -> bool {
    use crate::html_parser_state::{HTML_NAMESPACE, MATHML_NAMESPACE, SVG_NAMESPACE};

    let namespace = crate::jsdom::namespace_uri(node);
    if namespace == MATHML_NAMESPACE {
        return matches!(
            crate::dom::tag_name(node).as_str(),
            "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"
        );
    }
    if namespace == SVG_NAMESPACE {
        return matches!(
            crate::dom::tag_name(node).to_ascii_lowercase().as_str(),
            "foreignobject" | "desc" | "title"
        );
    }
    if namespace != HTML_NAMESPACE {
        return false;
    }
    matches!(
        crate::dom::tag_name(node).as_str(),
        "address"
            | "applet"
            | "area"
            | "article"
            | "aside"
            | "base"
            | "basefont"
            | "bgsound"
            | "blockquote"
            | "body"
            | "br"
            | "button"
            | "caption"
            | "center"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "embed"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "frame"
            | "frameset"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "iframe"
            | "img"
            | "input"
            | "li"
            | "link"
            | "listing"
            | "main"
            | "marquee"
            | "menu"
            | "meta"
            | "nav"
            | "noembed"
            | "noframes"
            | "noscript"
            | "object"
            | "ol"
            | "p"
            | "param"
            | "plaintext"
            | "pre"
            | "script"
            | "search"
            | "section"
            | "select"
            | "source"
            | "style"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "template"
            | "textarea"
            | "tfoot"
            | "th"
            | "thead"
            | "title"
            | "tr"
            | "track"
            | "ul"
            | "wbr"
            | "xmp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_policy_is_feature_neutral_and_covers_active_content() {
        assert!(is_active_fragment_element("script"));
        assert!(consumes_content_when_filtered("object"));
        assert!(is_html_void_element("img"));
        assert!(!is_html_void_element("div"));
        assert_eq!(find_raw_text_end("body</script >", "script"), Some(4));
        assert_eq!(find_raw_text_end("body</scripter>", "script"), None);
        assert!(is_unsafe_fragment_attribute("onclick", "run()"));
        assert!(is_unsafe_fragment_attribute(
            "xlink:href",
            "  JaVaScRiPt:run()"
        ));
        assert!(!is_unsafe_fragment_attribute(
            "href",
            "https://example.test/"
        ));
        let xlink = adjust_foreign_attribute(crate::html_parser_state::SVG_NAMESPACE, "xlink:href");
        assert_eq!(xlink.qualified_name, "xlink:href");
        assert_eq!(
            xlink.namespace,
            Some(crate::html_parser_state::XLINK_NAMESPACE)
        );
        assert_eq!(xlink.prefix, Some("xlink"));
        assert_eq!(xlink.local_name, "href");
        let html_xlink =
            adjust_foreign_attribute(crate::html_parser_state::HTML_NAMESPACE, "xlink:href");
        assert_eq!(html_xlink.namespace, None);
        assert_eq!(html_xlink.local_name, "xlink:href");
        let xmlns =
            adjust_foreign_attribute(crate::html_parser_state::SVG_NAMESPACE, "xmlns:xlink");
        assert_eq!(
            xmlns.namespace,
            Some(crate::html_parser_state::XMLNS_NAMESPACE)
        );
        assert_eq!(xmlns.local_name, "xlink");
    }
}
