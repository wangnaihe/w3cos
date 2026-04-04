use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use w3cos_std::style::Style;

use super::server::SerializedDocument;
use crate::layout::LayoutRect;

// ---------------------------------------------------------------------------
// CDP message envelope
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CdpRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize)]
pub struct CdpResponse {
    pub id: u64,
    pub result: Value,
}

#[derive(Serialize)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
}

// ---------------------------------------------------------------------------
// Highlight state (Overlay domain)
// ---------------------------------------------------------------------------

pub struct OverlayState {
    pub highlighted_node: Option<i64>,
    pub highlight_config: Option<HighlightConfig>,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            highlighted_node: None,
            highlight_config: None,
        }
    }
}

#[derive(Clone)]
pub struct HighlightConfig {
    pub content_color: [u8; 4],
    pub padding_color: [u8; 4],
    pub border_color: [u8; 4],
    pub margin_color: [u8; 4],
}

impl Default for HighlightConfig {
    fn default() -> Self {
        Self {
            content_color: [111, 168, 220, 102],
            padding_color: [147, 196, 125, 77],
            border_color: [255, 229, 153, 102],
            margin_color: [246, 178, 107, 77],
        }
    }
}

// ---------------------------------------------------------------------------
// CDP handler — dispatches methods to domain implementations
// ---------------------------------------------------------------------------

pub struct CdpHandler {
    pub overlay: OverlayState,
    enabled_domains: Vec<String>,
}

impl CdpHandler {
    pub fn new() -> Self {
        Self {
            overlay: OverlayState::default(),
            enabled_domains: Vec::new(),
        }
    }

    pub fn handle(
        &mut self,
        req: &CdpRequest,
        doc: &SerializedDocument,
        layout_rects: &[(LayoutRect, usize)],
    ) -> CdpResponse {
        let result = self.dispatch(&req.method, &req.params, doc, layout_rects);
        CdpResponse {
            id: req.id,
            result,
        }
    }

    fn dispatch(
        &mut self,
        method: &str,
        params: &Value,
        doc: &SerializedDocument,
        layout_rects: &[(LayoutRect, usize)],
    ) -> Value {
        match method {
            // --- Lifecycle ---
            "Target.getTargetInfo" => json!({
                "targetInfo": {
                    "targetId": "w3cos-main",
                    "type": "page",
                    "title": "W3C OS Application",
                    "url": "w3cos://app",
                    "attached": true,
                    "canAccessOpener": false
                }
            }),

            // --- DOM Domain ---
            "DOM.enable" => {
                self.enabled_domains.push("DOM".into());
                json!({})
            }
            "DOM.getDocument" => self.dom_get_document(doc),
            "DOM.requestChildNodes" => self.dom_request_child_nodes(params, doc),
            "DOM.querySelector" => self.dom_query_selector(params, doc),
            "DOM.querySelectorAll" => self.dom_query_selector_all(params, doc),
            "DOM.getOuterHTML" => self.dom_get_outer_html(params, doc),
            "DOM.getBoxModel" => self.dom_get_box_model(params, doc, layout_rects),
            "DOM.getNodeForLocation" => self.dom_get_node_for_location(params, layout_rects),
            "DOM.setAttributeValue" => json!({}),
            "DOM.removeAttribute" => json!({}),
            "DOM.setNodeValue" => json!({}),
            "DOM.pushNodesByBackendIdsToFrontend" => {
                let ids = params["backendNodeIds"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect::<Vec<_>>())
                    .unwrap_or_default();
                json!({ "nodeIds": ids })
            }
            "DOM.describeNode" => self.dom_describe_node(params, doc),
            "DOM.resolveNode" => json!({
                "object": {
                    "type": "object",
                    "subtype": "node",
                    "className": "HTMLElement",
                    "description": "HTMLElement"
                }
            }),
            "DOM.disable" => {
                self.enabled_domains.retain(|d| d != "DOM");
                json!({})
            }

            // --- CSS Domain ---
            "CSS.enable" => {
                self.enabled_domains.push("CSS".into());
                json!({})
            }
            "CSS.getComputedStyleForNode" => self.css_get_computed_style(params, doc),
            "CSS.getMatchedStylesForNode" => self.css_get_matched_styles(params, doc),
            "CSS.getInlineStylesForNode" => self.css_get_inline_styles(params, doc),
            "CSS.disable" => {
                self.enabled_domains.retain(|d| d != "CSS");
                json!({})
            }

            // --- Overlay Domain ---
            "Overlay.enable" => {
                self.enabled_domains.push("Overlay".into());
                json!({})
            }
            "Overlay.highlightNode" => {
                self.overlay_highlight_node(params);
                json!({})
            }
            "Overlay.hideHighlight" => {
                self.overlay.highlighted_node = None;
                self.overlay.highlight_config = None;
                json!({})
            }
            "Overlay.setInspectMode" => json!({}),
            "Overlay.disable" => {
                self.enabled_domains.retain(|d| d != "Overlay");
                self.overlay.highlighted_node = None;
                json!({})
            }

            // --- Page Domain ---
            "Page.enable" => {
                self.enabled_domains.push("Page".into());
                json!({})
            }
            "Page.getFrameTree" => json!({
                "frameTree": {
                    "frame": {
                        "id": "main",
                        "loaderId": "loader-1",
                        "url": "w3cos://app",
                        "domainAndRegistry": "w3cos",
                        "securityOrigin": "w3cos://app",
                        "mimeType": "text/html",
                        "adFrameStatus": { "adFrameType": "none" }
                    },
                    "childFrames": []
                }
            }),
            "Page.getResourceTree" => json!({
                "frameTree": {
                    "frame": {
                        "id": "main",
                        "loaderId": "loader-1",
                        "url": "w3cos://app",
                        "securityOrigin": "w3cos://app",
                        "mimeType": "text/html"
                    },
                    "resources": []
                }
            }),
            "Page.disable" => {
                self.enabled_domains.retain(|d| d != "Page");
                json!({})
            }

            // --- Runtime Domain (minimal) ---
            "Runtime.enable" => json!({}),
            "Runtime.disable" => json!({}),
            "Runtime.getIsolateId" => json!({ "id": "w3cos-isolate-1" }),
            "Runtime.runIfWaitingForDebugger" => json!({}),

            // --- Log / Network stubs ---
            "Log.enable" | "Log.disable" => json!({}),
            "Network.enable" | "Network.disable" => json!({}),
            "Security.enable" | "Security.disable" => json!({}),
            "Performance.enable" | "Performance.disable" => json!({}),
            "ServiceWorker.enable" | "ServiceWorker.disable" => json!({}),
            "Inspector.enable" | "Inspector.disable" => json!({}),
            "Debugger.enable" => json!({ "debuggerId": "w3cos-debugger-1" }),
            "Debugger.disable" => json!({}),
            "Debugger.setAsyncCallStackDepth" => json!({}),
            "Debugger.setBlackboxPatterns" => json!({}),
            "Profiler.enable" | "Profiler.disable" => json!({}),
            "HeapProfiler.enable" | "HeapProfiler.disable" => json!({}),

            // --- DOMStorage / IndexedDB stubs ---
            "DOMStorage.enable" | "DOMStorage.disable" => json!({}),
            "IndexedDB.enable" | "IndexedDB.disable" => json!({}),

            // --- Emulation stubs ---
            "Emulation.setDeviceMetricsOverride" => json!({}),
            "Emulation.setTouchEmulationEnabled" => json!({}),

            _ => {
                eprintln!("[DevTools] unhandled method: {method}");
                json!({})
            }
        }
    }

    // =======================================================================
    // DOM Domain
    // =======================================================================

    fn dom_get_document(&self, doc: &SerializedDocument) -> Value {
        let root_node = self.serialize_node(doc, 0, 2);
        json!({ "root": root_node })
    }

    fn dom_request_child_nodes(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let node_id = params["nodeId"].as_i64().unwrap_or(0);
        let depth = params["depth"].as_i64().unwrap_or(1);
        let children = self.serialize_children(doc, node_id as u32, depth as i32);
        json!({ "children": children })
    }

    fn dom_query_selector(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let selector = params["selector"].as_str().unwrap_or("");
        match self.query_selector_impl(doc, selector) {
            Some(id) => json!({ "nodeId": id as i64 }),
            None => json!({ "nodeId": 0 }),
        }
    }

    fn dom_query_selector_all(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let selector = params["selector"].as_str().unwrap_or("");
        let ids: Vec<i64> = self
            .query_selector_all_impl(doc, selector)
            .iter()
            .map(|&id| id as i64)
            .collect();
        json!({ "nodeIds": ids })
    }

    fn dom_get_outer_html(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let node_id = params["nodeId"].as_i64().unwrap_or(0);
        let html = self.build_outer_html(doc, node_id as u32);
        json!({ "outerHTML": html })
    }

    fn dom_get_box_model(
        &self,
        params: &Value,
        doc: &SerializedDocument,
        layout_rects: &[(LayoutRect, usize)],
    ) -> Value {
        let node_id = params["nodeId"].as_i64().unwrap_or(0) as usize;

        if let Some((rect, _)) = layout_rects.iter().find(|(_, idx)| *idx == node_id) {
            let style = doc
                .get_node(node_id as u32)
                .map(|n| &n.style)
                .cloned()
                .unwrap_or_default();
            let p = &style.padding;
            let m = &style.margin;
            let bw = style.border_width;

            let content = quad(
                rect.x + p.left + bw,
                rect.y + p.top + bw,
                rect.width - p.left - p.right - 2.0 * bw,
                rect.height - p.top - p.bottom - 2.0 * bw,
            );
            let padding = quad(
                rect.x + bw,
                rect.y + bw,
                rect.width - 2.0 * bw,
                rect.height - 2.0 * bw,
            );
            let border = quad(rect.x, rect.y, rect.width, rect.height);
            let margin = quad(
                rect.x - m.left,
                rect.y - m.top,
                rect.width + m.left + m.right,
                rect.height + m.top + m.bottom,
            );

            json!({
                "model": {
                    "content": content,
                    "padding": padding,
                    "border": border,
                    "margin": margin,
                    "width": rect.width as i64,
                    "height": rect.height as i64
                }
            })
        } else {
            json!({ "model": { "content": [0,0,0,0,0,0,0,0], "padding": [0,0,0,0,0,0,0,0], "border": [0,0,0,0,0,0,0,0], "margin": [0,0,0,0,0,0,0,0], "width": 0, "height": 0 }})
        }
    }

    fn dom_get_node_for_location(
        &self,
        params: &Value,
        layout_rects: &[(LayoutRect, usize)],
    ) -> Value {
        let x = params["x"].as_f64().unwrap_or(0.0) as f32;
        let y = params["y"].as_f64().unwrap_or(0.0) as f32;

        for (rect, idx) in layout_rects.iter().rev() {
            if x >= rect.x
                && x <= rect.x + rect.width
                && y >= rect.y
                && y <= rect.y + rect.height
            {
                return json!({
                    "backendNodeId": *idx as i64,
                    "frameId": "main",
                    "nodeId": *idx as i64
                });
            }
        }
        json!({ "backendNodeId": 1, "frameId": "main", "nodeId": 1 })
    }

    fn dom_describe_node(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let node_id = params["nodeId"]
            .as_i64()
            .or_else(|| params["backendNodeId"].as_i64())
            .unwrap_or(0);
        let node_data = self.serialize_node(doc, node_id as u32, 0);
        json!({ "node": node_data })
    }

    // =======================================================================
    // CSS Domain
    // =======================================================================

    fn css_get_computed_style(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let node_id = params["nodeId"].as_i64().unwrap_or(0);
        let style = doc
            .get_node(node_id as u32)
            .map(|n| &n.style)
            .cloned()
            .unwrap_or_default();
        let props = style_to_computed_properties(&style);
        json!({ "computedStyle": props })
    }

    fn css_get_matched_styles(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let node_id = params["nodeId"].as_i64().unwrap_or(0);
        let style = doc
            .get_node(node_id as u32)
            .map(|n| &n.style)
            .cloned()
            .unwrap_or_default();
        let css_props = style_to_css_properties(&style);

        json!({
            "inlineStyle": {
                "styleSheetId": "inline",
                "cssProperties": css_props,
                "shorthandEntries": []
            },
            "matchedCSSRules": [],
            "pseudoElements": [],
            "inherited": [],
            "cssKeyframesRules": []
        })
    }

    fn css_get_inline_styles(&self, params: &Value, doc: &SerializedDocument) -> Value {
        let node_id = params["nodeId"].as_i64().unwrap_or(0);
        let style = doc
            .get_node(node_id as u32)
            .map(|n| &n.style)
            .cloned()
            .unwrap_or_default();
        let css_props = style_to_css_properties(&style);

        json!({
            "inlineStyle": {
                "styleSheetId": "inline",
                "cssProperties": css_props,
                "shorthandEntries": []
            },
            "attributesStyle": {
                "cssProperties": [],
                "shorthandEntries": []
            }
        })
    }

    // =======================================================================
    // Overlay Domain
    // =======================================================================

    fn overlay_highlight_node(&mut self, params: &Value) {
        self.overlay.highlighted_node = params["nodeId"].as_i64();
        if let Some(cfg) = params.get("highlightConfig") {
            let mut hc = HighlightConfig::default();
            if let Some(c) = cfg.get("contentColor") {
                hc.content_color = parse_rgba_obj(c);
            }
            if let Some(c) = cfg.get("paddingColor") {
                hc.padding_color = parse_rgba_obj(c);
            }
            if let Some(c) = cfg.get("borderColor") {
                hc.border_color = parse_rgba_obj(c);
            }
            if let Some(c) = cfg.get("marginColor") {
                hc.margin_color = parse_rgba_obj(c);
            }
            self.overlay.highlight_config = Some(hc);
        }
    }

    // =======================================================================
    // Serialization helpers
    // =======================================================================

    fn serialize_node(&self, doc: &SerializedDocument, id: u32, depth: i32) -> Value {
        let node = match doc.get_node(id) {
            Some(n) => n,
            None => return json!({}),
        };
        let tag = &node.tag;
        let child_count = node.children.len();
        let node_type_num = node.node_type as i64;

        let mut attrs: Vec<String> = Vec::new();
        for (k, v) in &node.attributes {
            attrs.push(k.clone());
            attrs.push(v.clone());
        }
        if !node.class_list.is_empty() {
            attrs.push("class".to_string());
            attrs.push(node.class_list.join(" "));
        }

        let mut obj = json!({
            "nodeId": id as i64,
            "backendNodeId": id as i64,
            "nodeType": node_type_num,
            "nodeName": tag.to_uppercase(),
            "localName": tag,
            "nodeValue": node.text_content.as_deref().unwrap_or(""),
            "childNodeCount": child_count,
            "attributes": attrs,
        });

        if depth > 0 || (node_type_num == 9 && depth >= 0) {
            let children: Vec<Value> = node
                .children
                .iter()
                .map(|&cid| self.serialize_node(doc, cid, depth - 1))
                .collect();
            obj["children"] = json!(children);
        }

        if node_type_num == 9 {
            obj["documentURL"] = json!("w3cos://app");
            obj["baseURL"] = json!("w3cos://app");
            obj["xmlVersion"] = json!("");
            obj["compatibilityMode"] = json!("NoQuirksMode");
        }

        obj
    }

    fn serialize_children(&self, doc: &SerializedDocument, parent_id: u32, depth: i32) -> Vec<Value> {
        let node = match doc.get_node(parent_id) {
            Some(n) => n,
            None => return Vec::new(),
        };
        node.children
            .iter()
            .map(|&cid| self.serialize_node(doc, cid, depth))
            .collect()
    }

    fn build_outer_html(&self, doc: &SerializedDocument, id: u32) -> String {
        let node = match doc.get_node(id) {
            Some(n) => n,
            None => return String::new(),
        };
        let tag = &node.tag;

        if node.node_type == 3 {
            return node.text_content.clone().unwrap_or_default();
        }

        let mut html = format!("<{tag}");
        for (k, v) in &node.attributes {
            html.push_str(&format!(" {k}=\"{v}\""));
        }
        if !node.class_list.is_empty() {
            html.push_str(&format!(" class=\"{}\"", node.class_list.join(" ")));
        }
        html.push('>');
        if let Some(text) = &node.text_content {
            html.push_str(text);
        }
        for &cid in &node.children {
            html.push_str(&self.build_outer_html(doc, cid));
        }
        html.push_str(&format!("</{tag}>"));
        html
    }

    // Simple selector matching on the serialized document
    fn query_selector_impl(&self, doc: &SerializedDocument, selector: &str) -> Option<u32> {
        if let Some(id) = selector.strip_prefix('#') {
            return doc.nodes.iter().find(|n| {
                n.attributes.iter().any(|(k, v)| k == "id" && v == id)
            }).map(|n| n.id);
        }
        if let Some(class) = selector.strip_prefix('.') {
            return doc.nodes.iter().find(|n| {
                n.class_list.iter().any(|c| c == class)
            }).map(|n| n.id);
        }
        doc.nodes.iter().find(|n| n.tag == selector && n.node_type == 1).map(|n| n.id)
    }

    fn query_selector_all_impl(&self, doc: &SerializedDocument, selector: &str) -> Vec<u32> {
        if let Some(id) = selector.strip_prefix('#') {
            return doc.nodes.iter().filter(|n| {
                n.attributes.iter().any(|(k, v)| k == "id" && v == id)
            }).map(|n| n.id).collect();
        }
        if let Some(class) = selector.strip_prefix('.') {
            return doc.nodes.iter().filter(|n| {
                n.class_list.iter().any(|c| c == class)
            }).map(|n| n.id).collect();
        }
        doc.nodes.iter().filter(|n| n.tag == selector && n.node_type == 1).map(|n| n.id).collect()
    }
}

// ---------------------------------------------------------------------------
// Style → CDP computed/CSS properties
// ---------------------------------------------------------------------------

fn style_to_computed_properties(style: &Style) -> Vec<Value> {
    let mut props = Vec::new();
    let mut add = |name: &str, value: String| {
        props.push(json!({ "name": name, "value": value }));
    };

    add("display", format!("{:?}", style.display).to_lowercase());
    add("position", format!("{:?}", style.position).to_lowercase());
    add(
        "flex-direction",
        format!("{:?}", style.flex_direction).to_lowercase(),
    );
    add(
        "justify-content",
        format_justify(&style.justify_content),
    );
    add("align-items", format_align(&style.align_items));
    add("flex-wrap", format_flex_wrap(&style.flex_wrap));
    add("flex-grow", style.flex_grow.to_string());
    add("flex-shrink", style.flex_shrink.to_string());
    add("gap", format!("{}px", style.gap));
    add(
        "padding",
        format!(
            "{}px {}px {}px {}px",
            style.padding.top, style.padding.right, style.padding.bottom, style.padding.left
        ),
    );
    add(
        "margin",
        format!(
            "{}px {}px {}px {}px",
            style.margin.top, style.margin.right, style.margin.bottom, style.margin.left
        ),
    );
    add("width", format_dimension(&style.width));
    add("height", format_dimension(&style.height));
    add(
        "background-color",
        format!(
            "rgba({}, {}, {}, {})",
            style.background.r,
            style.background.g,
            style.background.b,
            style.background.a as f32 / 255.0
        ),
    );
    add(
        "color",
        format!(
            "rgba({}, {}, {}, {})",
            style.color.r,
            style.color.g,
            style.color.b,
            style.color.a as f32 / 255.0
        ),
    );
    add("font-size", format!("{}px", style.font_size));
    add("font-weight", style.font_weight.to_string());
    add("border-radius", format!("{}px", style.border_radius));
    add("border-width", format!("{}px", style.border_width));
    add(
        "border-color",
        format!(
            "rgba({}, {}, {}, {})",
            style.border_color.r,
            style.border_color.g,
            style.border_color.b,
            style.border_color.a as f32 / 255.0
        ),
    );
    add("opacity", style.opacity.to_string());
    add("overflow", format!("{:?}", style.overflow).to_lowercase());
    add("z-index", style.z_index.to_string());

    props
}

fn style_to_css_properties(style: &Style) -> Vec<Value> {
    let mut props = Vec::new();
    let mut add = |name: &str, value: String| {
        props.push(json!({
            "name": name,
            "value": value,
            "important": false,
            "implicit": false,
            "text": format!("{}: {};", name, value),
            "disabled": false,
            "parsedOk": true
        }));
    };

    let display = format!("{:?}", style.display).to_lowercase();
    if display != "flex" {
        add("display", display);
    }

    let pos = format!("{:?}", style.position).to_lowercase();
    if pos != "relative" {
        add("position", pos);
    }

    let dir = format!("{:?}", style.flex_direction).to_lowercase();
    if dir != "column" {
        add("flex-direction", dir);
    }

    let jc = format_justify(&style.justify_content);
    if jc != "flex-start" {
        add("justify-content", jc);
    }

    let ai = format_align(&style.align_items);
    if ai != "stretch" {
        add("align-items", ai);
    }

    if style.gap > 0.0 {
        add("gap", format!("{}px", style.gap));
    }
    if style.padding != w3cos_std::style::Edges::ZERO {
        add(
            "padding",
            format!(
                "{}px {}px {}px {}px",
                style.padding.top, style.padding.right, style.padding.bottom, style.padding.left
            ),
        );
    }
    if style.margin != w3cos_std::style::Edges::ZERO {
        add(
            "margin",
            format!(
                "{}px {}px {}px {}px",
                style.margin.top, style.margin.right, style.margin.bottom, style.margin.left
            ),
        );
    }
    if !matches!(style.width, w3cos_std::style::Dimension::Auto) {
        add("width", format_dimension(&style.width));
    }
    if !matches!(style.height, w3cos_std::style::Dimension::Auto) {
        add("height", format_dimension(&style.height));
    }
    if style.background.a > 0 {
        add(
            "background-color",
            format!(
                "rgba({}, {}, {}, {})",
                style.background.r,
                style.background.g,
                style.background.b,
                style.background.a as f32 / 255.0
            ),
        );
    }
    if style.color != w3cos_std::color::Color::WHITE {
        add(
            "color",
            format!(
                "rgba({}, {}, {}, {})",
                style.color.r, style.color.g, style.color.b, style.color.a as f32 / 255.0
            ),
        );
    }
    if style.font_size != 16.0 {
        add("font-size", format!("{}px", style.font_size));
    }
    if style.font_weight != 400 {
        add("font-weight", style.font_weight.to_string());
    }
    if style.border_radius > 0.0 {
        add("border-radius", format!("{}px", style.border_radius));
    }
    if style.border_width > 0.0 {
        add("border-width", format!("{}px", style.border_width));
    }
    if style.opacity != 1.0 {
        add("opacity", style.opacity.to_string());
    }

    props
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_dimension(d: &w3cos_std::style::Dimension) -> String {
    match d {
        w3cos_std::style::Dimension::Auto => "auto".into(),
        w3cos_std::style::Dimension::Px(v) => format!("{v}px"),
        w3cos_std::style::Dimension::Percent(v) => format!("{v}%"),
        w3cos_std::style::Dimension::Rem(v) => format!("{v}rem"),
        w3cos_std::style::Dimension::Em(v) => format!("{v}em"),
        w3cos_std::style::Dimension::Vw(v) => format!("{v}vw"),
        w3cos_std::style::Dimension::Vh(v) => format!("{v}vh"),
    }
}

fn format_justify(jc: &w3cos_std::style::JustifyContent) -> String {
    match jc {
        w3cos_std::style::JustifyContent::FlexStart => "flex-start".into(),
        w3cos_std::style::JustifyContent::FlexEnd => "flex-end".into(),
        w3cos_std::style::JustifyContent::Center => "center".into(),
        w3cos_std::style::JustifyContent::SpaceBetween => "space-between".into(),
        w3cos_std::style::JustifyContent::SpaceAround => "space-around".into(),
        w3cos_std::style::JustifyContent::SpaceEvenly => "space-evenly".into(),
    }
}

fn format_align(ai: &w3cos_std::style::AlignItems) -> String {
    match ai {
        w3cos_std::style::AlignItems::FlexStart => "flex-start".into(),
        w3cos_std::style::AlignItems::FlexEnd => "flex-end".into(),
        w3cos_std::style::AlignItems::Center => "center".into(),
        w3cos_std::style::AlignItems::Stretch => "stretch".into(),
        w3cos_std::style::AlignItems::Baseline => "baseline".into(),
    }
}

fn format_flex_wrap(fw: &w3cos_std::style::FlexWrap) -> String {
    match fw {
        w3cos_std::style::FlexWrap::NoWrap => "nowrap".into(),
        w3cos_std::style::FlexWrap::Wrap => "wrap".into(),
        w3cos_std::style::FlexWrap::WrapReverse => "wrap-reverse".into(),
    }
}

fn quad(x: f32, y: f32, w: f32, h: f32) -> Vec<f32> {
    vec![x, y, x + w, y, x + w, y + h, x, y + h]
}

fn parse_rgba_obj(v: &Value) -> [u8; 4] {
    let r = v["r"].as_u64().unwrap_or(0) as u8;
    let g = v["g"].as_u64().unwrap_or(0) as u8;
    let b = v["b"].as_u64().unwrap_or(0) as u8;
    let a = v["a"].as_f64().unwrap_or(1.0);
    [r, g, b, (a * 255.0) as u8]
}
