use crate::parser::StyleDecl;

#[derive(Debug, Clone)]
pub struct Stylesheet {
    pub layer_order: Vec<String>,
    pub rules: Vec<CssRule>,
}

impl Stylesheet {
    pub fn empty() -> Self {
        Self {
            layer_order: Vec::new(),
            rules: Vec::new(),
        }
    }

    pub fn merge(&mut self, other: Stylesheet) {
        for layer in other.layer_order {
            if !self.layer_order.contains(&layer) {
                self.layer_order.push(layer);
            }
        }
        self.rules.extend(other.rules);
    }
}

#[derive(Debug, Clone)]
pub struct CssRule {
    pub selectors: Vec<Selector>,
    pub style: StyleDecl,
    pub layer: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Selector {
    Universal,
    Element(String),
    Class(String),
    Compound {
        element: Option<String>,
        classes: Vec<String>,
    },
}

impl Selector {
    pub fn matches(&self, element_kind: &str, class_names: &[&str]) -> bool {
        match self {
            Selector::Universal => true,
            Selector::Element(e) => e == element_kind,
            Selector::Class(c) => class_names.contains(&c.as_str()),
            Selector::Compound { element, classes } => {
                if let Some(e) = element {
                    if e != element_kind {
                        return false;
                    }
                }
                classes.iter().all(|c| class_names.contains(&c.as_str()))
            }
        }
    }
}

pub fn parse_css(source: &str) -> Stylesheet {
    let source = strip_comments(source);
    let mut layer_order: Vec<String> = Vec::new();
    let mut anon_counter: u32 = 0;
    let rules = parse_block(&source, &mut layer_order, &mut anon_counter, None);
    Stylesheet { layer_order, rules }
}

/// Parse a block of CSS, which can be the top level or the inside of an @layer.
fn parse_block(
    source: &str,
    layer_order: &mut Vec<String>,
    anon_counter: &mut u32,
    current_layer: Option<&str>,
) -> Vec<CssRule> {
    let mut rules = Vec::new();
    let bytes = source.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        if bytes[pos] == b'@' {
            pos = parse_at_rule(
                source,
                pos,
                &mut rules,
                layer_order,
                anon_counter,
                current_layer,
            );
            continue;
        }

        // Normal rule: selectors { declarations }
        let selector_start = pos;
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let selector_str = source[selector_start..pos].trim();
        pos += 1;

        let (block_str, advance) = extract_brace_content(&source[pos..]);
        pos += advance;

        if !selector_str.is_empty() {
            let selectors = parse_selector_group(selector_str);
            if !selectors.is_empty() {
                let style = parse_declarations(block_str);
                rules.push(CssRule {
                    selectors,
                    style,
                    layer: current_layer.map(|s| s.to_string()),
                });
            }
        }
    }

    rules
}

fn parse_at_rule(
    source: &str,
    start: usize,
    rules: &mut Vec<CssRule>,
    layer_order: &mut Vec<String>,
    anon_counter: &mut u32,
    current_layer: Option<&str>,
) -> usize {
    let bytes = source.as_bytes();
    let mut pos = start + 1; // skip @

    let kw_start = pos;
    while pos < bytes.len() && bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'-' {
        pos += 1;
    }
    let keyword = &source[kw_start..pos];

    if keyword == "layer" {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            return pos;
        }

        if bytes[pos] == b'{' {
            // Anonymous layer: @layer { ... }
            let anon_name = format!("__anon_{anon_counter}");
            *anon_counter += 1;
            let full_name = qualify_layer_name(current_layer, &anon_name);
            push_layer_if_new(layer_order, &full_name);
            pos += 1;
            let (block_str, advance) = extract_brace_content(&source[pos..]);
            pos += advance;
            let inner = parse_block(block_str, layer_order, anon_counter, Some(&full_name));
            rules.extend(inner);
        } else if bytes[pos] == b';' {
            // Bare @layer; — do nothing
            pos += 1;
        } else {
            // Find ; or { to distinguish declaration vs block
            let scan_start = pos;
            let mut scan = pos;
            while scan < bytes.len() && bytes[scan] != b';' && bytes[scan] != b'{' {
                scan += 1;
            }
            if scan >= bytes.len() {
                return scan;
            }

            let name_part = source[scan_start..scan].trim();

            if bytes[scan] == b';' {
                // @layer name1, name2, name3;
                for name in name_part.split(',') {
                    let name = name.trim();
                    if !name.is_empty() {
                        let full = qualify_layer_name(current_layer, name);
                        push_layer_if_new(layer_order, &full);
                    }
                }
                pos = scan + 1;
            } else {
                // @layer name { ... }
                let full_name = if name_part.is_empty() {
                    let anon = format!("__anon_{anon_counter}");
                    *anon_counter += 1;
                    qualify_layer_name(current_layer, &anon)
                } else {
                    qualify_layer_name(current_layer, name_part)
                };
                push_layer_if_new(layer_order, &full_name);
                pos = scan + 1;
                let (block_str, advance) = extract_brace_content(&source[pos..]);
                pos += advance;
                let inner =
                    parse_block(block_str, layer_order, anon_counter, Some(&full_name));
                rules.extend(inner);
            }
        }
    } else {
        // Other @-rules (@media, @keyframes, etc.) — skip entirely
        let mut depth = 0i32;
        let mut found_brace = false;
        while pos < bytes.len() {
            if bytes[pos] == b'{' {
                depth += 1;
                found_brace = true;
            } else if bytes[pos] == b'}' {
                depth -= 1;
                if depth == 0 {
                    pos += 1;
                    break;
                }
            } else if !found_brace && bytes[pos] == b';' {
                pos += 1;
                break;
            }
            pos += 1;
        }
    }

    pos
}

fn qualify_layer_name(parent: Option<&str>, child: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{child}"),
        None => child.to_string(),
    }
}

fn push_layer_if_new(order: &mut Vec<String>, name: &str) {
    if !order.contains(&name.to_string()) {
        order.push(name.to_string());
    }
}

/// Extract the content between a `{` (already consumed) and its matching `}`.
/// Returns (content, bytes_consumed_including_closing_brace).
fn extract_brace_content(s: &str) -> (&str, usize) {
    let bytes = s.as_bytes();
    let mut depth = 1i32;
    let mut pos = 0;
    while pos < bytes.len() && depth > 0 {
        if bytes[pos] == b'{' {
            depth += 1;
        }
        if bytes[pos] == b'}' {
            depth -= 1;
        }
        if depth > 0 {
            pos += 1;
        }
    }
    let content = &s[..pos];
    let consumed = if pos < bytes.len() { pos + 1 } else { pos };
    (content, consumed)
}

fn strip_comments(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

fn parse_selector_group(s: &str) -> Vec<Selector> {
    s.split(',')
        .filter_map(|part| parse_single_selector(part.trim()))
        .collect()
}

fn parse_single_selector(s: &str) -> Option<Selector> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s == "*" {
        return Some(Selector::Universal);
    }

    let s = s
        .rsplit_once(|c: char| c.is_whitespace() || c == '>' || c == '+' || c == '~')
        .map(|(_, last)| last.trim())
        .unwrap_or(s);

    if s.is_empty() {
        return None;
    }

    let s = s.split(':').next().unwrap_or(s);
    if s.is_empty() {
        return None;
    }

    let parts: Vec<&str> = s.split('.').collect();

    let element = if parts[0].is_empty() {
        None
    } else {
        Some(parts[0].to_string())
    };

    let classes: Vec<String> = parts[1..]
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();

    if classes.is_empty() {
        element.map(Selector::Element)
    } else if element.is_none() && classes.len() == 1 {
        Some(Selector::Class(classes.into_iter().next().unwrap()))
    } else {
        Some(Selector::Compound { element, classes })
    }
}

fn parse_declarations(block: &str) -> StyleDecl {
    let mut style = StyleDecl::default();

    for decl in block.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }

        if let Some(colon_pos) = decl.find(':') {
            let property = decl[..colon_pos].trim();
            let value = decl[colon_pos + 1..].trim();
            let value = value.trim_end_matches("!important").trim();
            apply_css_property(&mut style, property, value);
        }
    }

    style
}

fn apply_css_property(style: &mut StyleDecl, property: &str, value: &str) {
    match property {
        "gap" => style.gap = css_parse_px(value),
        "padding" => style.padding = css_parse_px(value),
        "font-size" => style.font_size = css_parse_px(value),
        "font-weight" => style.font_weight = parse_font_weight(value),
        "color" => style.color = Some(value.to_string()),
        "background" | "background-color" => style.background = Some(value.to_string()),
        "border-radius" => style.border_radius = css_parse_px(value),
        "border-width" => style.border_width = css_parse_px(value),
        "border-color" => style.border_color = Some(value.to_string()),
        "align-items" => style.align_items = Some(value.to_string()),
        "justify-content" => style.justify_content = Some(value.to_string()),
        "width" => style.width = Some(value.to_string()),
        "height" => style.height = Some(value.to_string()),
        "flex-grow" => style.flex_grow = value.parse().ok(),
        "flex" => {
            if let Ok(v) = value.parse::<f32>() {
                style.flex_grow = Some(v);
            }
        }
        "position" => style.position = Some(value.to_string()),
        "top" => style.top = Some(value.to_string()),
        "right" => style.right = Some(value.to_string()),
        "bottom" => style.bottom = Some(value.to_string()),
        "left" => style.left = Some(value.to_string()),
        "z-index" => style.z_index = value.parse().ok(),
        "overflow" => style.overflow = Some(value.to_string()),
        "display" => style.display = Some(value.to_string()),
        "border" => parse_border_shorthand(style, value),
        _ => {}
    }
}

fn css_parse_px(value: &str) -> Option<f32> {
    let v = value.trim().trim_end_matches("px");
    v.parse().ok()
}

fn parse_font_weight(value: &str) -> Option<u16> {
    match value.trim() {
        "normal" => Some(400),
        "bold" => Some(700),
        "lighter" => Some(300),
        "bolder" => Some(800),
        _ => value.parse().ok(),
    }
}

fn parse_border_shorthand(style: &mut StyleDecl, value: &str) {
    let parts: Vec<&str> = value.split_whitespace().collect();
    for part in &parts {
        if let Some(px) = css_parse_px(part) {
            style.border_width = Some(px);
        } else if part.starts_with('#') || part.starts_with("rgb") {
            style.border_color = Some(part.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic parsing (unchanged) ──

    #[test]
    fn parse_simple_class_rule() {
        let css = ".title { color: #e94560; font-size: 24px; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 1);
        assert!(matches!(&sheet.rules[0].selectors[0], Selector::Class(c) if c == "title"));
        assert_eq!(sheet.rules[0].style.color.as_deref(), Some("#e94560"));
        assert_eq!(sheet.rules[0].style.font_size, Some(24.0));
        assert!(sheet.rules[0].layer.is_none());
    }

    #[test]
    fn parse_element_selector() {
        let css = "span { font-size: 16px; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 1);
        assert!(matches!(&sheet.rules[0].selectors[0], Selector::Element(e) if e == "span"));
    }

    #[test]
    fn parse_compound_selector() {
        let css = "span.highlight { color: yellow; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 1);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { element, classes } => {
                assert_eq!(element.as_deref(), Some("span"));
                assert_eq!(classes, &["highlight"]);
            }
            _ => panic!("expected Compound selector"),
        }
    }

    #[test]
    fn parse_multiple_selectors() {
        let css = ".a, .b { gap: 10; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].selectors.len(), 2);
    }

    #[test]
    fn parse_multiple_rules() {
        let css = r#"
            .container { padding: 16; background: #1e1e2e; }
            .title { font-size: 32; color: #ffffff; font-weight: bold; }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].style.padding, Some(16.0));
        assert_eq!(sheet.rules[1].style.font_size, Some(32.0));
        assert_eq!(sheet.rules[1].style.font_weight, Some(700));
    }

    #[test]
    fn parse_with_comments() {
        let css = r#"
            /* Main styles */
            .title { color: red; }
            // line comment (SCSS-style)
            .body { gap: 8; }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 2);
    }

    #[test]
    fn selector_matching() {
        let class_sel = Selector::Class("title".to_string());
        assert!(class_sel.matches("span", &["title"]));
        assert!(!class_sel.matches("span", &["body"]));

        let elem_sel = Selector::Element("span".to_string());
        assert!(elem_sel.matches("span", &[]));
        assert!(!elem_sel.matches("div", &[]));

        let compound = Selector::Compound {
            element: Some("span".to_string()),
            classes: vec!["title".to_string()],
        };
        assert!(compound.matches("span", &["title"]));
        assert!(!compound.matches("div", &["title"]));
        assert!(!compound.matches("span", &["body"]));
    }

    #[test]
    fn parse_border_shorthand_test() {
        let css = ".box { border: 2px solid #333; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules[0].style.border_width, Some(2.0));
        assert_eq!(sheet.rules[0].style.border_color.as_deref(), Some("#333"));
    }

    #[test]
    fn skip_at_rules() {
        let css = r#"
            @media (max-width: 600px) {
                .title { font-size: 18px; }
            }
            .body { gap: 8; }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].style.gap, Some(8.0));
    }

    #[test]
    fn parse_flex_shorthand() {
        let css = ".grow { flex: 1; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules[0].style.flex_grow, Some(1.0));
    }

    #[test]
    fn universal_selector() {
        let css = "* { gap: 4; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 1);
        assert!(matches!(
            &sheet.rules[0].selectors[0],
            Selector::Universal
        ));
        assert!(sheet.rules[0].selectors[0].matches("Anything", &[]));
    }

    // ── @layer tests ──

    #[test]
    fn layer_order_declaration() {
        let css = "@layer reset, base, components;";
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order, vec!["reset", "base", "components"]);
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn layer_block_with_rules() {
        let css = r#"
            @layer base {
                .title { font-size: 24; }
                .body { color: #333; }
            }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order, vec!["base"]);
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].layer.as_deref(), Some("base"));
        assert_eq!(sheet.rules[1].layer.as_deref(), Some("base"));
        assert_eq!(sheet.rules[0].style.font_size, Some(24.0));
    }

    #[test]
    fn layer_multiple_blocks() {
        let css = r#"
            @layer reset, base, components;

            @layer reset {
                * { gap: 0; padding: 0; }
            }

            @layer base {
                span { font-size: 16; color: #333; }
            }

            @layer components {
                .card { padding: 20; background: #fff; }
            }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order, vec!["reset", "base", "components"]);
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(sheet.rules[0].layer.as_deref(), Some("reset"));
        assert_eq!(sheet.rules[1].layer.as_deref(), Some("base"));
        assert_eq!(sheet.rules[2].layer.as_deref(), Some("components"));
    }

    #[test]
    fn layer_unlayered_rules_have_no_layer() {
        let css = r#"
            @layer base {
                .title { font-size: 24; }
            }
            .override { color: red; }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].layer.as_deref(), Some("base"));
        assert!(sheet.rules[1].layer.is_none());
    }

    #[test]
    fn layer_implicit_order_from_blocks() {
        let css = r#"
            @layer base {
                span { font-size: 16; }
            }
            @layer theme {
                span { color: #e94560; }
            }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order, vec!["base", "theme"]);
    }

    #[test]
    fn layer_nested() {
        let css = r#"
            @layer framework {
                @layer reset {
                    * { padding: 0; }
                }
                @layer base {
                    span { font-size: 14; }
                }
            }
        "#;
        let sheet = parse_css(css);
        assert_eq!(
            sheet.layer_order,
            vec!["framework", "framework.reset", "framework.base"]
        );
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(
            sheet.rules[0].layer.as_deref(),
            Some("framework.reset")
        );
        assert_eq!(
            sheet.rules[1].layer.as_deref(),
            Some("framework.base")
        );
    }

    #[test]
    fn layer_anonymous() {
        let css = r#"
            @layer {
                .anon { color: blue; }
            }
            .normal { color: red; }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order.len(), 1);
        assert!(sheet.layer_order[0].starts_with("__anon_"));
        assert_eq!(sheet.rules.len(), 2);
        assert!(sheet.rules[0].layer.is_some());
        assert!(sheet.rules[1].layer.is_none());
    }

    #[test]
    fn layer_order_dedup() {
        let css = r#"
            @layer a, b, a;
            @layer a {
                .x { color: red; }
            }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order, vec!["a", "b"]);
        assert_eq!(sheet.rules.len(), 1);
    }

    #[test]
    fn layer_with_media_skipped() {
        let css = r#"
            @layer base {
                .title { font-size: 24; }
            }
            @media (max-width: 600px) {
                .title { font-size: 18; }
            }
            @layer theme {
                .title { color: red; }
            }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order, vec!["base", "theme"]);
        assert_eq!(sheet.rules.len(), 2);
    }

    #[test]
    fn layer_dot_separated_name() {
        let css = r#"
            @layer framework.base {
                span { font-size: 14; }
            }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.layer_order, vec!["framework.base"]);
        assert_eq!(
            sheet.rules[0].layer.as_deref(),
            Some("framework.base")
        );
    }
}
