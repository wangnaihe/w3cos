//! DOM constructor identities and prototype chains for Value-backed nodes.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

pub const DOM_CONSTRUCTOR_NAMES: &[&str] = &[
    "Node",
    "Attr",
    "NamedNodeMap",
    "Location",
    "History",
    "Storage",
    "Performance",
    "PerformanceNavigation",
    "PerformanceTiming",
    "Crypto",
    "SubtleCrypto",
    "DOMStringList",
    "EventCounts",
    "ValidityState",
    "CharacterData",
    "Text",
    "CDATASection",
    "ProcessingInstruction",
    "Comment",
    "DocumentType",
    "DOMImplementation",
    "DOMStringMap",
    "DOMTokenList",
    "Document",
    "XMLDocument",
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
    "HTMLAreaElement",
    "HTMLAudioElement",
    "HTMLBRElement",
    "HTMLBaseElement",
    "HTMLBodyElement",
    "HTMLDListElement",
    "HTMLDataElement",
    "HTMLDataListElement",
    "HTMLDetailsElement",
    "HTMLDialogElement",
    "HTMLDirectoryElement",
    "HTMLEmbedElement",
    "HTMLFencedFrameElement",
    "HTMLFieldSetElement",
    "HTMLFontElement",
    "HTMLFrameElement",
    "HTMLFrameSetElement",
    "HTMLGeolocationElement",
    "HTMLHRElement",
    "HTMLHeadElement",
    "HTMLHeadingElement",
    "HTMLHtmlElement",
    "HTMLIFrameElement",
    "HTMLLIElement",
    "HTMLLabelElement",
    "HTMLLegendElement",
    "HTMLLinkElement",
    "HTMLMapElement",
    "HTMLMarqueeElement",
    "HTMLMediaElement",
    "HTMLMenuElement",
    "HTMLMetaElement",
    "HTMLMeterElement",
    "HTMLModElement",
    "HTMLOListElement",
    "HTMLObjectElement",
    "HTMLOptGroupElement",
    "HTMLOptionElement",
    "HTMLOutputElement",
    "HTMLParagraphElement",
    "HTMLParamElement",
    "HTMLPictureElement",
    "HTMLPreElement",
    "HTMLProgressElement",
    "HTMLQuoteElement",
    "HTMLScriptElement",
    "HTMLSelectedContentElement",
    "HTMLSlotElement",
    "HTMLSourceElement",
    "HTMLStyleElement",
    "HTMLTableCaptionElement",
    "HTMLTableCellElement",
    "HTMLTableColElement",
    "HTMLTableElement",
    "HTMLTableRowElement",
    "HTMLTableSectionElement",
    "HTMLTemplateElement",
    "HTMLTimeElement",
    "HTMLTitleElement",
    "HTMLTrackElement",
    "HTMLUListElement",
    "HTMLUnknownElement",
    "HTMLDocument",
    "HTMLAllCollection",
    "HTMLFormControlsCollection",
    "HTMLOptionsCollection",
    "SVGElement",
    "SVGSVGElement",
    "SVGGElement",
    "SVGPathElement",
    "SVGRectElement",
    "SVGCircleElement",
    "SVGEllipseElement",
    "SVGLineElement",
    "SVGPolylineElement",
    "SVGPolygonElement",
    "SVGTextElement",
    "SVGDefsElement",
    "SVGUseElement",
    "SVGAElement",
    "SVGAnimateElement",
    "SVGAnimateMotionElement",
    "SVGAnimateTransformElement",
    "SVGAnimationElement",
    "SVGClipPathElement",
    "SVGComponentTransferFunctionElement",
    "SVGDescElement",
    "SVGFEBlendElement",
    "SVGFEColorMatrixElement",
    "SVGFEComponentTransferElement",
    "SVGFECompositeElement",
    "SVGFEConvolveMatrixElement",
    "SVGFEDiffuseLightingElement",
    "SVGFEDisplacementMapElement",
    "SVGFEDistantLightElement",
    "SVGFEDropShadowElement",
    "SVGFEFloodElement",
    "SVGFEFuncAElement",
    "SVGFEFuncBElement",
    "SVGFEFuncGElement",
    "SVGFEFuncRElement",
    "SVGFEGaussianBlurElement",
    "SVGFEImageElement",
    "SVGFEMergeElement",
    "SVGFEMergeNodeElement",
    "SVGFEMorphologyElement",
    "SVGFEOffsetElement",
    "SVGFEPointLightElement",
    "SVGFESpecularLightingElement",
    "SVGFESpotLightElement",
    "SVGFETileElement",
    "SVGFETurbulenceElement",
    "SVGFilterElement",
    "SVGForeignObjectElement",
    "SVGGeometryElement",
    "SVGGradientElement",
    "SVGGraphicsElement",
    "SVGImageElement",
    "SVGLinearGradientElement",
    "SVGMPathElement",
    "SVGMarkerElement",
    "SVGMaskElement",
    "SVGMetadataElement",
    "SVGPatternElement",
    "SVGRadialGradientElement",
    "SVGScriptElement",
    "SVGSetElement",
    "SVGStopElement",
    "SVGStyleElement",
    "SVGSwitchElement",
    "SVGSymbolElement",
    "SVGTSpanElement",
    "SVGTextContentElement",
    "SVGTextPathElement",
    "SVGTextPositioningElement",
    "SVGTitleElement",
    "SVGViewElement",
    "DocumentFragment",
    "ShadowRoot",
    "AbstractRange",
    "Range",
    "StaticRange",
    "Selection",
    "TreeWalker",
    "NodeIterator",
];

thread_local! {
    static CONSTRUCTORS: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
}

fn parent_name(name: &str) -> Option<&'static str> {
    match name {
        "Document" | "Element" | "CharacterData" | "Attr" => Some("Node"),
        "Text" | "Comment" | "ProcessingInstruction" => Some("CharacterData"),
        "CDATASection" => Some("Text"),
        "DocumentType" => Some("Node"),
        "HTMLElement" => Some("Element"),
        "SVGElement" => Some("Element"),
        "SVGGraphicsElement" => Some("SVGElement"),
        "SVGGeometryElement" => Some("SVGGraphicsElement"),
        "SVGPathElement" | "SVGRectElement" | "SVGCircleElement" | "SVGEllipseElement"
        | "SVGLineElement" | "SVGPolylineElement" | "SVGPolygonElement" => {
            Some("SVGGeometryElement")
        }
        "SVGTextContentElement" => Some("SVGGraphicsElement"),
        "SVGTextPositioningElement" => Some("SVGTextContentElement"),
        "SVGTextElement" | "SVGTSpanElement" => Some("SVGTextPositioningElement"),
        "SVGAnimationElement" => Some("SVGElement"),
        "SVGAnimateElement"
        | "SVGAnimateMotionElement"
        | "SVGAnimateTransformElement"
        | "SVGSetElement" => Some("SVGAnimationElement"),
        "SVGGradientElement" => Some("SVGElement"),
        "SVGLinearGradientElement" | "SVGRadialGradientElement" => Some("SVGGradientElement"),
        "SVGSVGElement"
        | "SVGGElement"
        | "SVGDefsElement"
        | "SVGUseElement"
        | "SVGAElement"
        | "SVGClipPathElement"
        | "SVGComponentTransferFunctionElement"
        | "SVGDescElement"
        | "SVGFEBlendElement"
        | "SVGFEColorMatrixElement"
        | "SVGFEComponentTransferElement"
        | "SVGFECompositeElement"
        | "SVGFEConvolveMatrixElement"
        | "SVGFEDiffuseLightingElement"
        | "SVGFEDisplacementMapElement"
        | "SVGFEDistantLightElement"
        | "SVGFEDropShadowElement"
        | "SVGFEFloodElement"
        | "SVGFEFuncAElement"
        | "SVGFEFuncBElement"
        | "SVGFEFuncGElement"
        | "SVGFEFuncRElement"
        | "SVGFEGaussianBlurElement"
        | "SVGFEImageElement"
        | "SVGFEMergeElement"
        | "SVGFEMergeNodeElement"
        | "SVGFEMorphologyElement"
        | "SVGFEOffsetElement"
        | "SVGFEPointLightElement"
        | "SVGFESpecularLightingElement"
        | "SVGFESpotLightElement"
        | "SVGFETileElement"
        | "SVGFETurbulenceElement"
        | "SVGFilterElement"
        | "SVGForeignObjectElement"
        | "SVGImageElement"
        | "SVGMPathElement"
        | "SVGMarkerElement"
        | "SVGMaskElement"
        | "SVGMetadataElement"
        | "SVGPatternElement"
        | "SVGScriptElement"
        | "SVGStopElement"
        | "SVGStyleElement"
        | "SVGSwitchElement"
        | "SVGSymbolElement"
        | "SVGTextPathElement"
        | "SVGTitleElement"
        | "SVGViewElement" => Some("SVGElement"),
        "DocumentFragment" => Some("Node"),
        "HTMLDocument" | "XMLDocument" => Some("Document"),
        "HTMLAudioElement" => Some("HTMLMediaElement"),
        "HTMLAllCollection" | "HTMLFormControlsCollection" | "HTMLOptionsCollection" => None,
        "ShadowRoot" => Some("DocumentFragment"),
        "Range" | "StaticRange" => Some("AbstractRange"),
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
        "HTMLAreaElement"
        | "HTMLBRElement"
        | "HTMLBaseElement"
        | "HTMLBodyElement"
        | "HTMLDListElement"
        | "HTMLDataElement"
        | "HTMLDataListElement"
        | "HTMLDetailsElement"
        | "HTMLDialogElement"
        | "HTMLDirectoryElement"
        | "HTMLEmbedElement"
        | "HTMLFencedFrameElement"
        | "HTMLFieldSetElement"
        | "HTMLFontElement"
        | "HTMLFrameElement"
        | "HTMLFrameSetElement"
        | "HTMLGeolocationElement"
        | "HTMLHRElement"
        | "HTMLHeadElement"
        | "HTMLHeadingElement"
        | "HTMLHtmlElement"
        | "HTMLIFrameElement"
        | "HTMLLIElement"
        | "HTMLLabelElement"
        | "HTMLLegendElement"
        | "HTMLLinkElement"
        | "HTMLMapElement"
        | "HTMLMarqueeElement"
        | "HTMLMediaElement"
        | "HTMLMenuElement"
        | "HTMLMetaElement"
        | "HTMLMeterElement"
        | "HTMLModElement"
        | "HTMLOListElement"
        | "HTMLObjectElement"
        | "HTMLOptGroupElement"
        | "HTMLOptionElement"
        | "HTMLOutputElement"
        | "HTMLParagraphElement"
        | "HTMLParamElement"
        | "HTMLPictureElement"
        | "HTMLPreElement"
        | "HTMLProgressElement"
        | "HTMLQuoteElement"
        | "HTMLScriptElement"
        | "HTMLSelectedContentElement"
        | "HTMLSlotElement"
        | "HTMLSourceElement"
        | "HTMLStyleElement"
        | "HTMLTableCaptionElement"
        | "HTMLTableCellElement"
        | "HTMLTableColElement"
        | "HTMLTableElement"
        | "HTMLTableRowElement"
        | "HTMLTableSectionElement"
        | "HTMLTemplateElement"
        | "HTMLTimeElement"
        | "HTMLTitleElement"
        | "HTMLTrackElement"
        | "HTMLUListElement"
        | "HTMLUnknownElement" => Some("HTMLElement"),
        _ => None,
    }
}

fn build_constructors() -> HashMap<String, Value> {
    let mut constructors = HashMap::new();
    for name in DOM_CONSTRUCTOR_NAMES {
        let constructor = match *name {
            "Range" => Value::function(|_, _| crate::jsdom::range_value(0, 0, 0, 0)),
            "StaticRange" => Value::function(|_, args| crate::jsdom::static_range_value(args)),
            "Text" => Value::function(|_, args| crate::jsdom::text_value(args)),
            "Comment" => Value::function(|_, args| crate::jsdom::comment_value(args)),
            "AbstractRange"
            | "Attr"
            | "CharacterData"
            | "CDATASection"
            | "ProcessingInstruction"
            | "DocumentType"
            | "DOMImplementation"
            | "HTMLDocument"
            | "XMLDocument"
            | "DOMStringMap"
            | "DOMTokenList"
            | "Crypto"
            | "DOMStringList"
            | "EventCounts"
            | "History"
            | "Location"
            | "NamedNodeMap"
            | "Performance"
            | "PerformanceNavigation"
            | "PerformanceTiming"
            | "Storage"
            | "SubtleCrypto"
            | "ValidityState" => {
                let interface_name = *name;
                Value::function(move |_, _| {
                    w3cos_core::throw_value(Value::object(HashMap::from([
                        ("name".to_string(), Value::string("TypeError")),
                        (
                            "message".to_string(),
                            Value::string(&format!("Illegal constructor: {interface_name}")),
                        ),
                    ])))
                })
            }
            _ => Value::function(|_, _| Value::Undefined),
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

    for (constant, value) in [
        ("ELEMENT_NODE", 1.0),
        ("ATTRIBUTE_NODE", 2.0),
        ("TEXT_NODE", 3.0),
        ("CDATA_SECTION_NODE", 4.0),
        ("ENTITY_REFERENCE_NODE", 5.0),
        ("ENTITY_NODE", 6.0),
        ("PROCESSING_INSTRUCTION_NODE", 7.0),
        ("COMMENT_NODE", 8.0),
        ("DOCUMENT_NODE", 9.0),
        ("DOCUMENT_TYPE_NODE", 10.0),
        ("DOCUMENT_FRAGMENT_NODE", 11.0),
        ("NOTATION_NODE", 12.0),
        ("DOCUMENT_POSITION_DISCONNECTED", 1.0),
        ("DOCUMENT_POSITION_PRECEDING", 2.0),
        ("DOCUMENT_POSITION_FOLLOWING", 4.0),
        ("DOCUMENT_POSITION_CONTAINS", 8.0),
        ("DOCUMENT_POSITION_CONTAINED_BY", 16.0),
        ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32.0),
    ] {
        constructors["Node"].set_property(constant, Value::Number(value));
        constructors["Node"]
            .get_property("prototype")
            .set_property(constant, Value::Number(value));
    }
    for (constant, value) in [
        ("START_TO_START", 0.0),
        ("START_TO_END", 1.0),
        ("END_TO_END", 2.0),
        ("END_TO_START", 3.0),
    ] {
        constructors["Range"].set_property(constant, Value::Number(value));
        constructors["Range"]
            .get_property("prototype")
            .set_property(constant, Value::Number(value));
    }
    for (constant, value) in [
        ("SVG_ZOOMANDPAN_UNKNOWN", 0.0),
        ("SVG_ZOOMANDPAN_DISABLE", 1.0),
        ("SVG_ZOOMANDPAN_MAGNIFY", 2.0),
    ] {
        constructors["SVGSVGElement"].set_property(constant, Value::Number(value));
        constructors["SVGSVGElement"]
            .get_property("prototype")
            .set_property(constant, Value::Number(value));
    }

    let abstract_range = constructors["AbstractRange"].get_property("prototype");
    for property in [
        "startContainer",
        "startOffset",
        "endContainer",
        "endOffset",
        "collapsed",
    ] {
        abstract_range.set_property(property, Value::Undefined);
    }
    let node = constructors["Node"].get_property("prototype");
    for property in [
        "appendChild",
        "baseURI",
        "childNodes",
        "cloneNode",
        "compareDocumentPosition",
        "contains",
        "firstChild",
        "getRootNode",
        "hasChildNodes",
        "insertBefore",
        "isConnected",
        "isDefaultNamespace",
        "isEqualNode",
        "isSameNode",
        "lastChild",
        "lookupNamespaceURI",
        "lookupPrefix",
        "nextSibling",
        "nodeName",
        "nodeType",
        "nodeValue",
        "normalize",
        "ownerDocument",
        "parentElement",
        "parentNode",
        "previousSibling",
        "removeChild",
        "replaceChild",
        "textContent",
    ] {
        node.set_property(property, Value::Undefined);
    }
    let common_element_events = "onabort onanimationcancel onanimationend \
        onanimationiteration onanimationstart onauxclick onbeforeinput onbeforematch \
        onbeforetoggle onbeforexrselect onblur oncancel oncanplay oncanplaythrough onchange \
        onclick onclose oncommand oncontentvisibilityautostatechange oncontextlost \
        oncontextmenu oncontextrestored oncopy oncuechange oncut ondblclick ondrag ondragend \
        ondragenter ondragleave ondragover ondragstart ondrop ondurationchange onemptied onended \
        onerror onfocus onformdata ongotpointercapture oninput oninvalid onkeydown onkeypress \
        onkeyup onload onloadeddata onloadedmetadata onloadstart onlostpointercapture onmousedown \
        onmouseenter onmouseleave onmousemove onmouseout onmouseover onmouseup onmousewheel \
        onpaste onpause onplay onplaying onpointercancel onpointerdown onpointerenter \
        onpointerleave onpointermove onpointerout onpointerover onpointerrawupdate onpointerup \
        onprogress onratechange onreset onresize onscroll onscrollend onscrollsnapchange \
        onscrollsnapchanging onsecuritypolicyviolation onseeked onseeking onselect \
        onselectionchange onselectstart onslotchange onstalled onsubmit onsuspend ontimeupdate \
        ontoggle ontransitioncancel ontransitionend ontransitionrun ontransitionstart \
        onvolumechange onwaiting onwebkitanimationend onwebkitanimationiteration \
        onwebkitanimationstart onwebkittransitionend onwheel";
    for name in ["HTMLElement", "SVGElement", "Document"] {
        let prototype = constructors[name].get_property("prototype");
        for property in common_element_events.split_whitespace() {
            prototype.set_property(property, Value::Undefined);
        }
    }
    for (name, properties) in [
        (
            "HTMLElement",
            "accessKey attachInternals attributeStyleMap autocapitalize autofocus blur click \
             contentEditable dataset dir draggable editContext enterKeyHint focus focusGroup \
             focusGroupStart hidden hidePopover inert innerText inputMode isContentEditable lang \
             nonce offsetHeight offsetLeft offsetParent offsetTop offsetWidth outerText popover \
             showPopover spellcheck style tabIndex title togglePopover translate \
             virtualKeyboardPolicy writingSuggestions",
        ),
        (
            "SVGElement",
            "attributeStyleMap autofocus blur className dataset focus focusGroup focusGroupStart \
             nonce ownerSVGElement style tabIndex viewportElement",
        ),
        (
            "Element",
            "activeViewTransition after animate append ariaActiveDescendantElement ariaAtomic \
             ariaAutoComplete ariaBrailleLabel ariaBrailleRoleDescription ariaBusy ariaChecked \
             ariaColCount ariaColIndex ariaColIndexText ariaColSpan ariaControlsElements \
             ariaCurrent ariaDescribedByElements ariaDescription ariaDetailsElements \
             ariaDisabled ariaErrorMessageElements ariaExpanded ariaFlowToElements ariaHasPopup \
             ariaHidden ariaInvalid ariaKeyShortcuts ariaLabel ariaLabelledByElements ariaLevel \
             ariaLive ariaModal ariaMultiLine ariaMultiSelectable ariaNotify ariaOrientation \
             ariaPlaceholder ariaPosInSet ariaPressed ariaReadOnly ariaRelevant ariaRequired \
             ariaRoleDescription ariaRowCount ariaRowIndex ariaRowIndexText ariaRowSpan \
             ariaSelected ariaSetSize ariaSort ariaValueMax ariaValueMin ariaValueNow \
             ariaValueText assignedSlot attachShadow attributes before checkVisibility \
             childElementCount children classList className clientHeight clientLeft clientTop \
             clientWidth closest computedStyleMap currentCSSZoom customElementRegistry \
             elementTiming firstElementChild getAnimations getAttribute getAttributeNS \
             getAttributeNames getAttributeNode getAttributeNodeNS getBoundingClientRect \
             getClientRects getElementsByClassName getElementsByTagName getElementsByTagNameNS \
             getHTML hasAttribute hasAttributeNS hasAttributes hasPointerCapture id innerHTML \
             insertAdjacentElement insertAdjacentHTML insertAdjacentText lastElementChild \
             localName matches moveBefore namespaceURI nextElementSibling onbeforecopy \
             onbeforecut onbeforepaste onfullscreenchange onfullscreenerror onsearch \
             onwebkitfullscreenchange onwebkitfullscreenerror outerHTML part prefix prepend \
             previousElementSibling pseudo querySelector querySelectorAll releasePointerCapture \
             remove removeAttribute removeAttributeNS removeAttributeNode replaceChildren \
             replaceWith requestFullscreen requestPointerLock role scroll scrollBy scrollHeight \
             scrollIntoView scrollIntoViewIfNeeded scrollLeft scrollTo scrollTop scrollWidth \
             setAttribute setAttributeNS setAttributeNode setAttributeNodeNS setHTML setHTMLUnsafe \
             setPointerCapture shadowRoot slot startViewTransition tagName toggleAttribute \
             webkitMatchesSelector webkitRequestFullScreen webkitRequestFullscreen",
        ),
    ] {
        let prototype = constructors[name].get_property("prototype");
        for property in properties.split_whitespace() {
            prototype.set_property(property, Value::Undefined);
        }
    }
    for (name, members) in [
        (
            "SVGAElement",
            "download href hreflang interestForElement ping referrerPolicy rel relList target type",
        ),
        (
            "SVGAnimationElement",
            "beginElement beginElementAt endElement endElementAt getCurrentTime \
             getSimpleDuration getStartTime onbegin onend onrepeat requiredExtensions \
             systemLanguage targetElement",
        ),
        ("SVGClipPathElement", "clipPathUnits transform"),
        (
            "SVGComponentTransferFunctionElement",
            "amplitude exponent intercept offset slope tableValues type",
        ),
        ("SVGFEBlendElement", "height in1 in2 mode result width x y"),
        (
            "SVGFEColorMatrixElement",
            "height in1 result type values width x y",
        ),
        (
            "SVGFEComponentTransferElement",
            "height in1 result width x y",
        ),
        (
            "SVGFECompositeElement",
            "height in1 in2 k1 k2 k3 k4 operator result width x y",
        ),
        (
            "SVGFEConvolveMatrixElement",
            "bias divisor edgeMode height in1 kernelMatrix kernelUnitLengthX kernelUnitLengthY \
             orderX orderY preserveAlpha result targetX targetY width x y",
        ),
        (
            "SVGFEDiffuseLightingElement",
            "diffuseConstant height in1 kernelUnitLengthX kernelUnitLengthY result surfaceScale \
             width x y",
        ),
        (
            "SVGFEDisplacementMapElement",
            "height in1 in2 result scale width x xChannelSelector y yChannelSelector",
        ),
        ("SVGFEDistantLightElement", "azimuth elevation"),
        (
            "SVGFEDropShadowElement",
            "dx dy height in1 result setStdDeviation stdDeviationX stdDeviationY width x y",
        ),
        ("SVGFEFloodElement", "height result width x y"),
        (
            "SVGFEGaussianBlurElement",
            "height in1 result setStdDeviation stdDeviationX stdDeviationY width x y",
        ),
        (
            "SVGFEImageElement",
            "height href preserveAspectRatio result width x y",
        ),
        ("SVGFEMergeElement", "height result width x y"),
        ("SVGFEMergeNodeElement", "in1"),
        (
            "SVGFEMorphologyElement",
            "height in1 operator radiusX radiusY result width x y",
        ),
        ("SVGFEOffsetElement", "dx dy height in1 result width x y"),
        ("SVGFEPointLightElement", "x y z"),
        (
            "SVGFESpecularLightingElement",
            "height in1 kernelUnitLengthX kernelUnitLengthY result specularConstant \
             specularExponent surfaceScale width x y",
        ),
        (
            "SVGFESpotLightElement",
            "limitingConeAngle pointsAtX pointsAtY pointsAtZ specularExponent x y z",
        ),
        ("SVGFETileElement", "height in1 result width x y"),
        (
            "SVGFETurbulenceElement",
            "baseFrequencyX baseFrequencyY height numOctaves result seed stitchTiles type width x y",
        ),
        (
            "SVGFilterElement",
            "filterUnits height href primitiveUnits width x y",
        ),
        ("SVGForeignObjectElement", "height width x y"),
        (
            "SVGGeometryElement",
            "getPointAtLength getTotalLength isPointInFill isPointInStroke pathLength",
        ),
        (
            "SVGGradientElement",
            "gradientTransform gradientUnits href spreadMethod",
        ),
        (
            "SVGGraphicsElement",
            "farthestViewportElement getBBox getCTM getScreenCTM nearestViewportElement \
             requiredExtensions systemLanguage transform",
        ),
        (
            "SVGImageElement",
            "crossOrigin decode decoding height href preserveAspectRatio width x y",
        ),
        ("SVGLinearGradientElement", "x1 x2 y1 y2"),
        ("SVGMPathElement", "href"),
        (
            "SVGMarkerElement",
            "markerHeight markerUnits markerWidth orientAngle orientType preserveAspectRatio \
             refX refY setOrientToAngle setOrientToAuto viewBox",
        ),
        (
            "SVGMaskElement",
            "height maskContentUnits maskUnits requiredExtensions systemLanguage width x y",
        ),
        (
            "SVGPatternElement",
            "height href patternContentUnits patternTransform patternUnits preserveAspectRatio \
             requiredExtensions systemLanguage viewBox width x y",
        ),
        ("SVGRadialGradientElement", "cx cy fr fx fy r"),
        ("SVGScriptElement", "async href type"),
        ("SVGStopElement", "offset"),
        ("SVGStyleElement", "disabled media sheet title type"),
        ("SVGSymbolElement", "preserveAspectRatio viewBox"),
        (
            "SVGTextContentElement",
            "getCharNumAtPosition getComputedTextLength getEndPositionOfChar getExtentOfChar \
             getNumberOfChars getRotationOfChar getStartPositionOfChar getSubStringLength \
             lengthAdjust selectSubString textLength",
        ),
        ("SVGTextPathElement", "href method spacing startOffset"),
        ("SVGTextPositioningElement", "dx dy rotate x y"),
        ("SVGViewElement", "preserveAspectRatio viewBox zoomAndPan"),
    ] {
        let prototype = constructors[name].get_property("prototype");
        for member in members.split_whitespace() {
            prototype.set_property(member, Value::Undefined);
        }
    }
    for (name, constants) in [
        (
            "SVGComponentTransferFunctionElement",
            "SVG_FECOMPONENTTRANSFER_TYPE_UNKNOWN:0 \
             SVG_FECOMPONENTTRANSFER_TYPE_IDENTITY:1 SVG_FECOMPONENTTRANSFER_TYPE_TABLE:2 \
             SVG_FECOMPONENTTRANSFER_TYPE_DISCRETE:3 SVG_FECOMPONENTTRANSFER_TYPE_LINEAR:4 \
             SVG_FECOMPONENTTRANSFER_TYPE_GAMMA:5",
        ),
        (
            "SVGFEBlendElement",
            "SVG_FEBLEND_MODE_UNKNOWN:0 SVG_FEBLEND_MODE_NORMAL:1 \
             SVG_FEBLEND_MODE_MULTIPLY:2 SVG_FEBLEND_MODE_SCREEN:3 \
             SVG_FEBLEND_MODE_DARKEN:4 SVG_FEBLEND_MODE_LIGHTEN:5 \
             SVG_FEBLEND_MODE_OVERLAY:6 SVG_FEBLEND_MODE_COLOR_DODGE:7 \
             SVG_FEBLEND_MODE_COLOR_BURN:8 SVG_FEBLEND_MODE_HARD_LIGHT:9 \
             SVG_FEBLEND_MODE_SOFT_LIGHT:10 SVG_FEBLEND_MODE_DIFFERENCE:11 \
             SVG_FEBLEND_MODE_EXCLUSION:12 SVG_FEBLEND_MODE_HUE:13 \
             SVG_FEBLEND_MODE_SATURATION:14 SVG_FEBLEND_MODE_COLOR:15 \
             SVG_FEBLEND_MODE_LUMINOSITY:16",
        ),
        (
            "SVGFEColorMatrixElement",
            "SVG_FECOLORMATRIX_TYPE_UNKNOWN:0 SVG_FECOLORMATRIX_TYPE_MATRIX:1 \
             SVG_FECOLORMATRIX_TYPE_SATURATE:2 SVG_FECOLORMATRIX_TYPE_HUEROTATE:3 \
             SVG_FECOLORMATRIX_TYPE_LUMINANCETOALPHA:4",
        ),
        (
            "SVGFECompositeElement",
            "SVG_FECOMPOSITE_OPERATOR_UNKNOWN:0 SVG_FECOMPOSITE_OPERATOR_OVER:1 \
             SVG_FECOMPOSITE_OPERATOR_IN:2 SVG_FECOMPOSITE_OPERATOR_OUT:3 \
             SVG_FECOMPOSITE_OPERATOR_ATOP:4 SVG_FECOMPOSITE_OPERATOR_XOR:5 \
             SVG_FECOMPOSITE_OPERATOR_ARITHMETIC:6",
        ),
        (
            "SVGFEConvolveMatrixElement",
            "SVG_EDGEMODE_UNKNOWN:0 SVG_EDGEMODE_DUPLICATE:1 SVG_EDGEMODE_WRAP:2 \
             SVG_EDGEMODE_NONE:3",
        ),
        (
            "SVGFEDisplacementMapElement",
            "SVG_CHANNEL_UNKNOWN:0 SVG_CHANNEL_R:1 SVG_CHANNEL_G:2 SVG_CHANNEL_B:3 SVG_CHANNEL_A:4",
        ),
        (
            "SVGFEMorphologyElement",
            "SVG_MORPHOLOGY_OPERATOR_UNKNOWN:0 SVG_MORPHOLOGY_OPERATOR_ERODE:1 \
             SVG_MORPHOLOGY_OPERATOR_DILATE:2",
        ),
        (
            "SVGFETurbulenceElement",
            "SVG_STITCHTYPE_UNKNOWN:0 SVG_STITCHTYPE_STITCH:1 SVG_STITCHTYPE_NOSTITCH:2 \
             SVG_TURBULENCE_TYPE_UNKNOWN:0 SVG_TURBULENCE_TYPE_TURBULENCE:1 \
             SVG_TURBULENCE_TYPE_FRACTALNOISE:2",
        ),
        (
            "SVGGradientElement",
            "SVG_SPREADMETHOD_UNKNOWN:0 SVG_SPREADMETHOD_PAD:1 SVG_SPREADMETHOD_REFLECT:2 \
             SVG_SPREADMETHOD_REPEAT:3",
        ),
        (
            "SVGMarkerElement",
            "SVG_MARKERUNITS_UNKNOWN:0 SVG_MARKERUNITS_USERSPACEONUSE:1 \
             SVG_MARKERUNITS_STROKEWIDTH:2 SVG_MARKER_ORIENT_UNKNOWN:0 \
             SVG_MARKER_ORIENT_AUTO:1 SVG_MARKER_ORIENT_ANGLE:2",
        ),
        (
            "SVGTextContentElement",
            "LENGTHADJUST_UNKNOWN:0 LENGTHADJUST_SPACING:1 LENGTHADJUST_SPACINGANDGLYPHS:2",
        ),
        (
            "SVGTextPathElement",
            "TEXTPATH_METHODTYPE_UNKNOWN:0 TEXTPATH_METHODTYPE_ALIGN:1 \
             TEXTPATH_METHODTYPE_STRETCH:2 TEXTPATH_SPACINGTYPE_UNKNOWN:0 \
             TEXTPATH_SPACINGTYPE_AUTO:1 TEXTPATH_SPACINGTYPE_EXACT:2",
        ),
        (
            "SVGViewElement",
            "SVG_ZOOMANDPAN_UNKNOWN:0 SVG_ZOOMANDPAN_DISABLE:1 SVG_ZOOMANDPAN_MAGNIFY:2",
        ),
    ] {
        let prototype = constructors[name].get_property("prototype");
        for constant in constants.split_whitespace() {
            let (constant, value) = constant
                .split_once(':')
                .expect("SVG constant manifest must include a numeric value");
            let value = Value::Number(
                value
                    .parse::<f64>()
                    .expect("SVG constant manifest value must be numeric"),
            );
            constructors[name].set_property(constant, value.clone());
            prototype.set_property(constant, value);
        }
    }
    let document = constructors["Document"].get_property("prototype");
    for property in "URL activeElement activeViewTransition adoptNode adoptedStyleSheets \
        alinkColor all anchors append applets ariaNotify bgColor body browsingTopics \
        captureEvents caretPositionFromPoint caretRangeFromPoint characterSet charset \
        childElementCount children clear close compatMode contentType cookie createAttribute \
        createAttributeNS createCDATASection createComment createDocumentFragment createElement \
        createElementNS createEvent createExpression createNSResolver createNodeIterator \
        createProcessingInstruction createRange createTextNode createTreeWalker currentScript \
        customElementRegistry defaultView designMode dir doctype documentElement documentURI \
        domain elementFromPoint elementsFromPoint embeds evaluate execCommand exitFullscreen \
        exitPictureInPicture exitPointerLock featurePolicy fgColor firstElementChild fonts forms \
        fullscreen fullscreenElement fullscreenEnabled getAnimations getElementById \
        getElementsByClassName getElementsByName getElementsByTagName getElementsByTagNameNS \
        getSelection hasFocus hasPrivateToken hasRedemptionRecord hasStorageAccess \
        hasUnpartitionedCookieAccess head images implementation importNode inputEncoding \
        lastElementChild lastModified linkColor links moveBefore onbeforecopy onbeforecut \
        onbeforepaste onfreeze onfullscreenchange onfullscreenerror onpointerlockchange \
        onpointerlockerror onprerenderingchange onreadystatechange onresume onsearch \
        onwebkitfullscreenchange onwebkitfullscreenerror open pictureInPictureElement \
        pictureInPictureEnabled plugins pointerLockElement prepend prerendering \
        queryCommandEnabled queryCommandIndeterm queryCommandState queryCommandSupported \
        queryCommandValue querySelector querySelectorAll readyState referrer releaseEvents \
        replaceChildren requestStorageAccess requestStorageAccessFor rootElement scripts \
        scrollingElement startViewTransition styleSheets timeline title vlinkColor wasDiscarded \
        webkitCancelFullScreen webkitCurrentFullScreenElement webkitExitFullscreen \
        webkitFullscreenElement webkitFullscreenEnabled webkitHidden webkitIsFullScreen \
        webkitVisibilityState write writeln xmlEncoding xmlStandalone xmlVersion"
        .split_whitespace()
    {
        document.set_property(property, Value::Undefined);
    }
    for (name, members) in [
        ("HTMLAllCollection", "item length namedItem"),
        (
            "HTMLAreaElement",
            "alt attributionSrc coords download hash host hostname href interestForElement noHref \
             origin password pathname ping port protocol referrerPolicy rel relList search shape \
             target toString username",
        ),
        ("HTMLBRElement", "clear"),
        ("HTMLBaseElement", "href target"),
        (
            "HTMLBodyElement",
            "aLink background bgColor link onafterprint onbeforeprint onbeforeunload onblur \
             onerror onfocus ongamepadconnected ongamepaddisconnected onhashchange \
             onlanguagechange onload onmessage onmessageerror onoffline ononline onpagehide \
             onpageshow onpopstate onrejectionhandled onresize onscroll onstorage \
             onunhandledrejection onunload text vLink",
        ),
        ("HTMLDListElement", "compact"),
        ("HTMLDataElement", "value"),
        ("HTMLDataListElement", "options"),
        ("HTMLDetailsElement", "name open"),
        (
            "HTMLDialogElement",
            "close closedBy open requestClose returnValue show showModal",
        ),
        ("HTMLDirectoryElement", "compact"),
        (
            "HTMLEmbedElement",
            "align getSVGDocument height name src type width",
        ),
        (
            "HTMLFencedFrameElement",
            "allow config height sandbox width",
        ),
        (
            "HTMLFieldSetElement",
            "checkValidity disabled elements form name reportValidity setCustomValidity type \
             validationMessage validity willValidate",
        ),
        ("HTMLFontElement", "color face size"),
        ("HTMLFormControlsCollection", "namedItem"),
        (
            "HTMLFrameElement",
            "contentDocument contentWindow frameBorder longDesc marginHeight marginWidth name \
             noResize scrolling src",
        ),
        (
            "HTMLFrameSetElement",
            "cols onafterprint onbeforeprint onbeforeunload onblur onerror onfocus \
             ongamepadconnected ongamepaddisconnected onhashchange onlanguagechange onload \
             onmessage onmessageerror onoffline ononline onpagehide onpageshow onpopstate \
             onrejectionhandled onresize onscroll onstorage onunhandledrejection onunload rows",
        ),
        (
            "HTMLGeolocationElement",
            "accuracymode autolocate error initialPermissionStatus invalidReason isValid \
             onlocation onpromptaction onpromptdismiss onvalidationstatuschange \
             permissionStatus position watch",
        ),
        ("HTMLHRElement", "align color noShade size width"),
        ("HTMLHeadingElement", "align"),
        ("HTMLHtmlElement", "version"),
        (
            "HTMLIFrameElement",
            "adAuctionHeaders align allow allowFullscreen allowPaymentRequest browsingTopics \
             contentDocument contentWindow credentialless csp featurePolicy frameBorder \
             getSVGDocument height loading longDesc marginHeight marginWidth name privateToken \
             referrerPolicy sandbox scrolling sharedStorageWritable src srcdoc width",
        ),
        ("HTMLLIElement", "type value"),
        ("HTMLLabelElement", "control form htmlFor"),
        ("HTMLLegendElement", "align form"),
        (
            "HTMLLinkElement",
            "as blocking charset crossOrigin disabled fetchPriority href hreflang imageSizes \
             imageSrcset integrity media referrerPolicy rel relList rev sheet sizes target type",
        ),
        ("HTMLMapElement", "areas name"),
        (
            "HTMLMarqueeElement",
            "behavior bgColor direction height hspace loop scrollAmount scrollDelay start stop \
             trueSpeed vspace width",
        ),
        (
            "HTMLMediaElement",
            "HAVE_CURRENT_DATA HAVE_ENOUGH_DATA HAVE_FUTURE_DATA HAVE_METADATA HAVE_NOTHING \
             NETWORK_EMPTY NETWORK_IDLE NETWORK_LOADING NETWORK_NO_SOURCE addTextTrack autoplay \
             buffered canPlayType captureStream controls controlsList crossOrigin currentSrc \
             currentTime defaultMuted defaultPlaybackRate disableRemotePlayback duration ended \
             error load loading loop mediaKeys muted networkState onencrypted onwaitingforkey \
             pause paused play playbackRate played preload preservesPitch readyState remote \
             seekable seeking setMediaKeys setSinkId sinkId src srcObject textTracks volume \
             webkitAudioDecodedByteCount webkitVideoDecodedByteCount",
        ),
        ("HTMLMenuElement", "compact"),
        ("HTMLMetaElement", "content httpEquiv media name scheme"),
        ("HTMLMeterElement", "high labels low max min optimum value"),
        ("HTMLModElement", "cite dateTime"),
        ("HTMLOListElement", "compact reversed start type"),
        (
            "HTMLObjectElement",
            "align archive border checkValidity code codeBase codeType contentDocument \
             contentWindow data declare form getSVGDocument height hspace name reportValidity \
             setCustomValidity standby type useMap validationMessage validity vspace width \
             willValidate",
        ),
        ("HTMLOptGroupElement", "disabled label"),
        (
            "HTMLOptionElement",
            "defaultSelected disabled form index label selected text value",
        ),
        ("HTMLOptionsCollection", "add length remove selectedIndex"),
        (
            "HTMLOutputElement",
            "checkValidity defaultValue form htmlFor labels name reportValidity \
             setCustomValidity type validationMessage validity value willValidate",
        ),
        ("HTMLParagraphElement", "align"),
        ("HTMLParamElement", "name type value valueType"),
        ("HTMLPreElement", "width"),
        ("HTMLProgressElement", "labels max position value"),
        ("HTMLQuoteElement", "cite"),
        (
            "HTMLScriptElement",
            "async attributionSrc blocking charset crossOrigin defer event fetchPriority htmlFor \
             innerText integrity noModule referrerPolicy src text textContent type",
        ),
        (
            "HTMLSlotElement",
            "assign assignedElements assignedNodes name",
        ),
        (
            "HTMLSourceElement",
            "height media sizes src srcset type width",
        ),
        ("HTMLStyleElement", "blocking disabled media sheet type"),
        ("HTMLTableCaptionElement", "align"),
        (
            "HTMLTableCellElement",
            "abbr align axis bgColor cellIndex ch chOff colSpan headers height noWrap rowSpan \
             scope vAlign width",
        ),
        ("HTMLTableColElement", "align ch chOff span vAlign width"),
        (
            "HTMLTableElement",
            "align bgColor border caption cellPadding cellSpacing createCaption createTBody \
             createTFoot createTHead deleteCaption deleteRow deleteTFoot deleteTHead frame \
             insertRow rows rules summary tBodies tFoot tHead width",
        ),
        (
            "HTMLTableRowElement",
            "align bgColor cells ch chOff deleteCell insertCell rowIndex sectionRowIndex vAlign",
        ),
        (
            "HTMLTableSectionElement",
            "align ch chOff deleteRow insertRow rows vAlign",
        ),
        (
            "HTMLTemplateElement",
            "content htmlFor shadowRootClonable shadowRootCustomElementRegistry \
             shadowRootDelegatesFocus shadowRootMode shadowRootSerializable",
        ),
        ("HTMLTimeElement", "dateTime"),
        ("HTMLTitleElement", "text"),
        (
            "HTMLTrackElement",
            "ERROR LOADED LOADING NONE default kind label readyState src srclang track",
        ),
        ("HTMLUListElement", "compact type"),
    ] {
        let prototype = constructors[name].get_property("prototype");
        for member in members.split_whitespace() {
            prototype.set_property(member, Value::Undefined);
        }
    }
    for (name, value) in [
        ("NETWORK_EMPTY", 0.0),
        ("NETWORK_IDLE", 1.0),
        ("NETWORK_LOADING", 2.0),
        ("NETWORK_NO_SOURCE", 3.0),
        ("HAVE_NOTHING", 0.0),
        ("HAVE_METADATA", 1.0),
        ("HAVE_CURRENT_DATA", 2.0),
        ("HAVE_FUTURE_DATA", 3.0),
        ("HAVE_ENOUGH_DATA", 4.0),
    ] {
        constructors["HTMLMediaElement"].set_property(name, Value::Number(value));
        constructors["HTMLMediaElement"]
            .get_property("prototype")
            .set_property(name, Value::Number(value));
    }
    for (name, value) in [
        ("NONE", 0.0),
        ("LOADING", 1.0),
        ("LOADED", 2.0),
        ("ERROR", 3.0),
    ] {
        constructors["HTMLTrackElement"].set_property(name, Value::Number(value));
        constructors["HTMLTrackElement"]
            .get_property("prototype")
            .set_property(name, Value::Number(value));
    }
    constructors["HTMLFencedFrameElement"].set_property(
        "canLoadOpaqueURL",
        Value::function(|_, _| Value::Bool(false)),
    );
    constructors["HTMLScriptElement"].set_property(
        "supports",
        Value::function(|_, args| {
            Value::Bool(matches!(
                args.first()
                    .map(Value::to_js_string)
                    .unwrap_or_default()
                    .as_str(),
                "classic" | "module" | "importmap" | "speculationrules"
            ))
        }),
    );
    let character_data = constructors["CharacterData"].get_property("prototype");
    for property in [
        "after",
        "appendData",
        "before",
        "data",
        "deleteData",
        "insertData",
        "length",
        "nextElementSibling",
        "previousElementSibling",
        "remove",
        "replaceData",
        "replaceWith",
        "substringData",
    ] {
        character_data.set_property(property, Value::Undefined);
    }
    let text = constructors["Text"].get_property("prototype");
    for property in ["assignedSlot", "splitText", "wholeText"] {
        text.set_property(property, Value::Undefined);
    }
    let processing_instruction = constructors["ProcessingInstruction"].get_property("prototype");
    // Chrome 150 also exposes the Element-style attribute helpers directly on
    // this prototype. They are retained for inventory compatibility even
    // though the DOM standard only requires `sheet` and `target`.
    for property in [
        "getAttribute",
        "getAttributeNames",
        "hasAttribute",
        "hasAttributes",
        "removeAttribute",
        "setAttribute",
        "sheet",
        "target",
        "toggleAttribute",
    ] {
        processing_instruction.set_property(property, Value::Undefined);
    }
    let document_type = constructors["DocumentType"].get_property("prototype");
    for property in [
        "after",
        "before",
        "name",
        "publicId",
        "remove",
        "replaceWith",
        "systemId",
    ] {
        document_type.set_property(property, Value::Undefined);
    }
    let implementation = constructors["DOMImplementation"].get_property("prototype");
    for property in [
        "createDocument",
        "createDocumentType",
        "createHTMLDocument",
        "hasFeature",
    ] {
        implementation.set_property(property, Value::Undefined);
    }
    let token_list = constructors["DOMTokenList"].get_property("prototype");
    for property in [
        "add", "contains", "entries", "forEach", "item", "keys", "length", "remove", "replace",
        "supports", "toString", "toggle", "value", "values",
    ] {
        token_list.set_property(property, Value::Undefined);
    }
    let attr = constructors["Attr"].get_property("prototype");
    for property in [
        "localName",
        "name",
        "namespaceURI",
        "ownerElement",
        "prefix",
        "specified",
        "value",
    ] {
        attr.set_property(property, Value::Undefined);
    }
    let named_node_map = constructors["NamedNodeMap"].get_property("prototype");
    for property in [
        "getNamedItem",
        "getNamedItemNS",
        "item",
        "length",
        "removeNamedItem",
        "removeNamedItemNS",
        "setNamedItem",
        "setNamedItemNS",
    ] {
        named_node_map.set_property(property, Value::Undefined);
    }
    let location = constructors["Location"].get_property("prototype");
    for property in [
        "ancestorOrigins",
        "assign",
        "hash",
        "host",
        "hostname",
        "href",
        "origin",
        "pathname",
        "port",
        "protocol",
        "reload",
        "replace",
        "search",
        "toString",
    ] {
        location.set_property(property, Value::Undefined);
    }
    let history = constructors["History"].get_property("prototype");
    for property in [
        "back",
        "forward",
        "go",
        "length",
        "pushState",
        "replaceState",
        "scrollRestoration",
        "state",
    ] {
        history.set_property(property, Value::Undefined);
    }
    let storage = constructors["Storage"].get_property("prototype");
    for property in ["clear", "getItem", "key", "length", "removeItem", "setItem"] {
        storage.set_property(property, Value::Undefined);
    }
    let performance = constructors["Performance"].get_property("prototype");
    for property in [
        "clearMarks",
        "clearMeasures",
        "clearResourceTimings",
        "eventCounts",
        "getEntries",
        "getEntriesByName",
        "getEntriesByType",
        "interactionCount",
        "mark",
        "measure",
        "measureUserAgentSpecificMemory",
        "memory",
        "navigation",
        "now",
        "onresourcetimingbufferfull",
        "setResourceTimingBufferSize",
        "timeOrigin",
        "timing",
        "toJSON",
    ] {
        performance.set_property(property, Value::Undefined);
    }
    w3cos_core::class::set_prototype_of(
        &performance,
        &crate::web_events::event_target_class().get_property("prototype"),
    );
    let performance_navigation = constructors["PerformanceNavigation"].get_property("prototype");
    for property in [
        "TYPE_BACK_FORWARD",
        "TYPE_NAVIGATE",
        "TYPE_RELOAD",
        "TYPE_RESERVED",
        "redirectCount",
        "toJSON",
        "type",
    ] {
        performance_navigation.set_property(property, Value::Undefined);
    }
    for (constant, value) in [
        ("TYPE_NAVIGATE", 0.0),
        ("TYPE_RELOAD", 1.0),
        ("TYPE_BACK_FORWARD", 2.0),
        ("TYPE_RESERVED", 255.0),
    ] {
        constructors["PerformanceNavigation"].set_property(constant, Value::Number(value));
        performance_navigation.set_property(constant, Value::Number(value));
    }
    let performance_timing = constructors["PerformanceTiming"].get_property("prototype");
    for property in [
        "connectEnd",
        "connectStart",
        "domComplete",
        "domContentLoadedEventEnd",
        "domContentLoadedEventStart",
        "domInteractive",
        "domLoading",
        "domainLookupEnd",
        "domainLookupStart",
        "fetchStart",
        "loadEventEnd",
        "loadEventStart",
        "navigationStart",
        "redirectEnd",
        "redirectStart",
        "requestStart",
        "responseEnd",
        "responseStart",
        "secureConnectionStart",
        "toJSON",
        "unloadEventEnd",
        "unloadEventStart",
    ] {
        performance_timing.set_property(property, Value::Undefined);
    }
    let crypto = constructors["Crypto"].get_property("prototype");
    for property in ["getRandomValues", "randomUUID", "subtle"] {
        crypto.set_property(property, Value::Undefined);
    }
    let subtle_crypto = constructors["SubtleCrypto"].get_property("prototype");
    for property in [
        "decrypt",
        "deriveBits",
        "deriveKey",
        "digest",
        "encrypt",
        "exportKey",
        "generateKey",
        "importKey",
        "sign",
        "unwrapKey",
        "verify",
        "wrapKey",
    ] {
        subtle_crypto.set_property(property, Value::Undefined);
    }
    let string_list = constructors["DOMStringList"].get_property("prototype");
    for property in ["contains", "item", "length"] {
        string_list.set_property(property, Value::Undefined);
    }
    let event_counts = constructors["EventCounts"].get_property("prototype");
    for property in ["size", "entries", "forEach", "get", "has", "keys", "values"] {
        event_counts.set_property(property, Value::Undefined);
    }
    let validity_state = constructors["ValidityState"].get_property("prototype");
    for property in [
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
        validity_state.set_property(property, Value::Undefined);
    }
    for name in [
        "HTMLInputElement",
        "HTMLTextAreaElement",
        "HTMLSelectElement",
    ] {
        let prototype = constructors[name].get_property("prototype");
        for property in [
            "checkValidity",
            "reportValidity",
            "setCustomValidity",
            "validationMessage",
            "validity",
            "willValidate",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
    }
    let form = constructors["HTMLFormElement"].get_property("prototype");
    for property in ["checkValidity", "reportValidity"] {
        form.set_property(property, Value::Undefined);
    }
    for (name, properties) in [
        (
            "HTMLCanvasElement",
            &[
                "captureStream",
                "getContext",
                "height",
                "toBlob",
                "toDataURL",
                "transferControlToOffscreen",
                "width",
            ][..],
        ),
        (
            "NodeIterator",
            &[
                "detach",
                "filter",
                "nextNode",
                "pointerBeforeReferenceNode",
                "previousNode",
                "referenceNode",
                "root",
                "whatToShow",
            ][..],
        ),
        (
            "TreeWalker",
            &[
                "currentNode",
                "filter",
                "firstChild",
                "lastChild",
                "nextNode",
                "nextSibling",
                "parentNode",
                "previousNode",
                "previousSibling",
                "root",
                "whatToShow",
            ][..],
        ),
        (
            "DocumentFragment",
            &[
                "append",
                "childElementCount",
                "children",
                "firstElementChild",
                "getElementById",
                "lastElementChild",
                "moveBefore",
                "prepend",
                "querySelector",
                "querySelectorAll",
                "replaceChildren",
            ][..],
        ),
        (
            "Range",
            &[
                "cloneContents",
                "cloneRange",
                "collapse",
                "commonAncestorContainer",
                "compareBoundaryPoints",
                "comparePoint",
                "createContextualFragment",
                "deleteContents",
                "detach",
                "expand",
                "extractContents",
                "getBoundingClientRect",
                "getClientRects",
                "insertNode",
                "intersectsNode",
                "isPointInRange",
                "selectNode",
                "selectNodeContents",
                "setEnd",
                "setEndAfter",
                "setEndBefore",
                "setStart",
                "setStartAfter",
                "setStartBefore",
                "surroundContents",
                "toString",
            ][..],
        ),
        (
            "Selection",
            &[
                "addRange",
                "anchorNode",
                "anchorOffset",
                "baseNode",
                "baseOffset",
                "collapse",
                "collapseToEnd",
                "collapseToStart",
                "containsNode",
                "deleteFromDocument",
                "direction",
                "empty",
                "extend",
                "extentNode",
                "extentOffset",
                "focusNode",
                "focusOffset",
                "getComposedRanges",
                "getRangeAt",
                "isCollapsed",
                "modify",
                "rangeCount",
                "removeAllRanges",
                "removeRange",
                "selectAllChildren",
                "setBaseAndExtent",
                "setPosition",
                "toString",
                "type",
            ][..],
        ),
        (
            "ShadowRoot",
            &[
                "activeElement",
                "adoptedStyleSheets",
                "clonable",
                "customElementRegistry",
                "delegatesFocus",
                "elementFromPoint",
                "elementsFromPoint",
                "fullscreenElement",
                "getAnimations",
                "getHTML",
                "getSelection",
                "host",
                "innerHTML",
                "mode",
                "onslotchange",
                "pictureInPictureElement",
                "pointerLockElement",
                "serializable",
                "setHTML",
                "setHTMLUnsafe",
                "slotAssignment",
                "styleSheets",
            ][..],
        ),
        (
            "HTMLVideoElement",
            &[
                "cancelVideoFrameCallback",
                "disablePictureInPicture",
                "getVideoPlaybackQuality",
                "height",
                "onenterpictureinpicture",
                "onleavepictureinpicture",
                "playsInline",
                "poster",
                "requestPictureInPicture",
                "requestVideoFrameCallback",
                "videoHeight",
                "videoWidth",
                "webkitDecodedFrameCount",
                "webkitDroppedFrameCount",
                "width",
            ][..],
        ),
        (
            "HTMLFormElement",
            &[
                "acceptCharset",
                "action",
                "autocomplete",
                "elements",
                "encoding",
                "enctype",
                "length",
                "method",
                "name",
                "noValidate",
                "rel",
                "relList",
                "requestSubmit",
                "reset",
                "submit",
                "target",
            ][..],
        ),
        (
            "HTMLSelectElement",
            &[
                "add",
                "autocomplete",
                "disabled",
                "form",
                "item",
                "labels",
                "length",
                "multiple",
                "name",
                "namedItem",
                "options",
                "remove",
                "required",
                "selectedIndex",
                "selectedOptions",
                "showPicker",
                "size",
                "type",
                "value",
            ][..],
        ),
        (
            "HTMLButtonElement",
            &[
                "checkValidity",
                "command",
                "commandForElement",
                "disabled",
                "form",
                "formAction",
                "formEnctype",
                "formMethod",
                "formNoValidate",
                "formTarget",
                "interestForElement",
                "labels",
                "name",
                "popoverTargetAction",
                "popoverTargetElement",
                "reportValidity",
                "setCustomValidity",
                "type",
                "validationMessage",
                "validity",
                "value",
                "willValidate",
            ][..],
        ),
        (
            "HTMLTextAreaElement",
            &[
                "autocomplete",
                "cols",
                "defaultValue",
                "dirName",
                "disabled",
                "form",
                "labels",
                "maxLength",
                "minLength",
                "name",
                "placeholder",
                "readOnly",
                "required",
                "rows",
                "select",
                "selectionDirection",
                "selectionEnd",
                "selectionStart",
                "setRangeText",
                "setSelectionRange",
                "textLength",
                "type",
                "value",
                "wrap",
            ][..],
        ),
        (
            "HTMLAnchorElement",
            &[
                "attributionSrc",
                "charset",
                "coords",
                "download",
                "hash",
                "host",
                "hostname",
                "href",
                "hrefTranslate",
                "hreflang",
                "interestForElement",
                "name",
                "origin",
                "password",
                "pathname",
                "ping",
                "port",
                "protocol",
                "referrerPolicy",
                "rel",
                "relList",
                "rev",
                "search",
                "shape",
                "target",
                "text",
                "toString",
                "type",
                "username",
            ][..],
        ),
        (
            "HTMLImageElement",
            &[
                "align",
                "alt",
                "attributionSrc",
                "border",
                "browsingTopics",
                "complete",
                "crossOrigin",
                "currentSrc",
                "decode",
                "decoding",
                "fetchPriority",
                "height",
                "hspace",
                "isMap",
                "loading",
                "longDesc",
                "lowsrc",
                "name",
                "naturalHeight",
                "naturalWidth",
                "referrerPolicy",
                "sharedStorageWritable",
                "sizes",
                "src",
                "srcset",
                "useMap",
                "vspace",
                "width",
                "x",
                "y",
            ][..],
        ),
        (
            "HTMLInputElement",
            &[
                "accept",
                "align",
                "alt",
                "autocomplete",
                "checked",
                "defaultChecked",
                "defaultValue",
                "dirName",
                "disabled",
                "files",
                "form",
                "formAction",
                "formEnctype",
                "formMethod",
                "formNoValidate",
                "formTarget",
                "height",
                "incremental",
                "indeterminate",
                "labels",
                "list",
                "max",
                "maxLength",
                "min",
                "minLength",
                "multiple",
                "name",
                "pattern",
                "placeholder",
                "popoverTargetAction",
                "popoverTargetElement",
                "readOnly",
                "required",
                "select",
                "selectionDirection",
                "selectionEnd",
                "selectionStart",
                "setRangeText",
                "setSelectionRange",
                "showPicker",
                "size",
                "src",
                "step",
                "stepDown",
                "stepUp",
                "type",
                "useMap",
                "value",
                "valueAsDate",
                "valueAsNumber",
                "webkitEntries",
                "webkitdirectory",
                "width",
            ][..],
        ),
    ] {
        let prototype = constructors[name].get_property("prototype");
        for property in properties {
            prototype.set_property(property, Value::Undefined);
        }
    }
    constructors["HTMLDivElement"]
        .get_property("prototype")
        .set_property("align", Value::Undefined);
    let svg_root = constructors["SVGSVGElement"].get_property("prototype");
    for property in [
        "animationsPaused",
        "checkEnclosure",
        "checkIntersection",
        "createSVGAngle",
        "createSVGLength",
        "createSVGMatrix",
        "createSVGNumber",
        "createSVGPoint",
        "createSVGRect",
        "createSVGTransform",
        "createSVGTransformFromMatrix",
        "currentScale",
        "currentTranslate",
        "deselectAll",
        "forceRedraw",
        "getCurrentTime",
        "getElementById",
        "getEnclosureList",
        "getIntersectionList",
        "height",
        "pauseAnimations",
        "preserveAspectRatio",
        "setCurrentTime",
        "suspendRedraw",
        "unpauseAnimations",
        "unsuspendRedraw",
        "unsuspendRedrawAll",
        "viewBox",
        "width",
        "x",
        "y",
        "zoomAndPan",
    ] {
        svg_root.set_property(property, Value::Undefined);
    }
    for (name, properties) in [
        ("SVGCircleElement", &["cx", "cy", "r"][..]),
        ("SVGEllipseElement", &["cx", "cy", "rx", "ry"][..]),
        ("SVGLineElement", &["x1", "x2", "y1", "y2"][..]),
        ("SVGPolygonElement", &["animatedPoints", "points"][..]),
        ("SVGPolylineElement", &["animatedPoints", "points"][..]),
        (
            "SVGRectElement",
            &["height", "rx", "ry", "width", "x", "y"][..],
        ),
        ("SVGUseElement", &["height", "href", "width", "x", "y"][..]),
    ] {
        let prototype = constructors[name].get_property("prototype");
        for property in properties {
            prototype.set_property(property, Value::Undefined);
        }
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
        "area" => "HTMLAreaElement",
        "audio" => "HTMLAudioElement",
        "br" => "HTMLBRElement",
        "base" => "HTMLBaseElement",
        "body" => "HTMLBodyElement",
        "dl" => "HTMLDListElement",
        "data" => "HTMLDataElement",
        "datalist" => "HTMLDataListElement",
        "details" => "HTMLDetailsElement",
        "dialog" => "HTMLDialogElement",
        "dir" => "HTMLDirectoryElement",
        "embed" => "HTMLEmbedElement",
        "fencedframe" => "HTMLFencedFrameElement",
        "fieldset" => "HTMLFieldSetElement",
        "font" => "HTMLFontElement",
        "frame" => "HTMLFrameElement",
        "frameset" => "HTMLFrameSetElement",
        "geolocation" => "HTMLGeolocationElement",
        "hr" => "HTMLHRElement",
        "head" => "HTMLHeadElement",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "HTMLHeadingElement",
        "html" => "HTMLHtmlElement",
        "iframe" => "HTMLIFrameElement",
        "li" => "HTMLLIElement",
        "label" => "HTMLLabelElement",
        "legend" => "HTMLLegendElement",
        "link" => "HTMLLinkElement",
        "map" => "HTMLMapElement",
        "marquee" => "HTMLMarqueeElement",
        "menu" => "HTMLMenuElement",
        "meta" => "HTMLMetaElement",
        "meter" => "HTMLMeterElement",
        "del" | "ins" => "HTMLModElement",
        "ol" => "HTMLOListElement",
        "object" => "HTMLObjectElement",
        "optgroup" => "HTMLOptGroupElement",
        "option" => "HTMLOptionElement",
        "output" => "HTMLOutputElement",
        "p" => "HTMLParagraphElement",
        "param" => "HTMLParamElement",
        "picture" => "HTMLPictureElement",
        "pre" | "listing" | "xmp" => "HTMLPreElement",
        "progress" => "HTMLProgressElement",
        "q" | "blockquote" => "HTMLQuoteElement",
        "script" => "HTMLScriptElement",
        "selectedcontent" => "HTMLSelectedContentElement",
        "slot" => "HTMLSlotElement",
        "source" => "HTMLSourceElement",
        "style" => "HTMLStyleElement",
        "caption" => "HTMLTableCaptionElement",
        "td" | "th" => "HTMLTableCellElement",
        "col" | "colgroup" => "HTMLTableColElement",
        "table" => "HTMLTableElement",
        "tr" => "HTMLTableRowElement",
        "thead" | "tbody" | "tfoot" => "HTMLTableSectionElement",
        "template" => "HTMLTemplateElement",
        "time" => "HTMLTimeElement",
        "title" => "HTMLTitleElement",
        "track" => "HTMLTrackElement",
        "ul" => "HTMLUListElement",
        "canvas" => "HTMLCanvasElement",
        _ => "HTMLElement",
    }
}

fn svg_constructor_for_tag(tag: &str) -> &'static str {
    match tag {
        "a" => "SVGAElement",
        "animate" => "SVGAnimateElement",
        "animatemotion" => "SVGAnimateMotionElement",
        "animatetransform" => "SVGAnimateTransformElement",
        "clippath" => "SVGClipPathElement",
        "desc" => "SVGDescElement",
        "feblend" => "SVGFEBlendElement",
        "fecolormatrix" => "SVGFEColorMatrixElement",
        "fecomponenttransfer" => "SVGFEComponentTransferElement",
        "fecomposite" => "SVGFECompositeElement",
        "feconvolvematrix" => "SVGFEConvolveMatrixElement",
        "fediffuselighting" => "SVGFEDiffuseLightingElement",
        "fedisplacementmap" => "SVGFEDisplacementMapElement",
        "fedistantlight" => "SVGFEDistantLightElement",
        "fedropshadow" => "SVGFEDropShadowElement",
        "feflood" => "SVGFEFloodElement",
        "fefunca" => "SVGFEFuncAElement",
        "fefuncb" => "SVGFEFuncBElement",
        "fefuncg" => "SVGFEFuncGElement",
        "fefuncr" => "SVGFEFuncRElement",
        "fegaussianblur" => "SVGFEGaussianBlurElement",
        "feimage" => "SVGFEImageElement",
        "femerge" => "SVGFEMergeElement",
        "femergenode" => "SVGFEMergeNodeElement",
        "femorphology" => "SVGFEMorphologyElement",
        "feoffset" => "SVGFEOffsetElement",
        "fepointlight" => "SVGFEPointLightElement",
        "fespecularlighting" => "SVGFESpecularLightingElement",
        "fespotlight" => "SVGFESpotLightElement",
        "fetile" => "SVGFETileElement",
        "feturbulence" => "SVGFETurbulenceElement",
        "filter" => "SVGFilterElement",
        "foreignobject" => "SVGForeignObjectElement",
        "image" => "SVGImageElement",
        "lineargradient" => "SVGLinearGradientElement",
        "mpath" => "SVGMPathElement",
        "marker" => "SVGMarkerElement",
        "mask" => "SVGMaskElement",
        "metadata" => "SVGMetadataElement",
        "pattern" => "SVGPatternElement",
        "radialgradient" => "SVGRadialGradientElement",
        "script" => "SVGScriptElement",
        "set" => "SVGSetElement",
        "stop" => "SVGStopElement",
        "style" => "SVGStyleElement",
        "switch" => "SVGSwitchElement",
        "symbol" => "SVGSymbolElement",
        "tspan" => "SVGTSpanElement",
        "textpath" => "SVGTextPathElement",
        "title" => "SVGTitleElement",
        "view" => "SVGViewElement",
        "svg" => "SVGSVGElement",
        "g" => "SVGGElement",
        "path" => "SVGPathElement",
        "rect" => "SVGRectElement",
        "circle" => "SVGCircleElement",
        "ellipse" => "SVGEllipseElement",
        "line" => "SVGLineElement",
        "polyline" => "SVGPolylineElement",
        "polygon" => "SVGPolygonElement",
        "text" => "SVGTextElement",
        "defs" => "SVGDefsElement",
        "use" => "SVGUseElement",
        _ => "SVGElement",
    }
}

pub fn prototype_for_node(node_type: u16, tag: &str, is_svg: bool) -> Value {
    match node_type {
        1 if is_svg => prototype(svg_constructor_for_tag(tag)),
        1 => prototype(html_constructor_for_tag(tag)),
        3 => prototype("Text"),
        4 => prototype("CDATASection"),
        7 => prototype("ProcessingInstruction"),
        8 => prototype("Comment"),
        10 => prototype("DocumentType"),
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
