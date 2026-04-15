use crate::css_parser::Stylesheet;
use crate::parser::{Node, NodeKind, StyleDecl};

/// DOM tag name — the standard HTML element name for this node.
pub fn dom_tag(node: &Node) -> &'static str {
    match &node.kind {
        NodeKind::Column | NodeKind::Row | NodeKind::Box => "div",
        NodeKind::Text(_) => "span",
        NodeKind::Button(_) => "button",
        NodeKind::Image(_) => "img",
        NodeKind::TextInput => "input",
    }
}

/// Component name — the W3COS component type (kept as alias for convenience).
fn component_name(node: &Node) -> &'static str {
    match &node.kind {
        NodeKind::Column => "Column",
        NodeKind::Row => "Row",
        NodeKind::Text(_) => "Text",
        NodeKind::Button(_) => "Button",
        NodeKind::Box => "Box",
        NodeKind::Image(_) => "Image",
        NodeKind::TextInput => "TextInput",
    }
}

/// Resolve the final style for a node by merging CSS rules and inline styles.
///
/// Element selectors match against **DOM tag names** (`div`, `span`, `button`,
/// `img`, `input`) following the W3C CSS spec. Component names (`Column`,
/// `Text`, etc.) are also accepted as aliases for convenience.
///
/// **Cascade precedence** (low → high):
/// 1. Earlier `@layer` rules
/// 2. Later `@layer` rules
/// 3. Un-layered CSS rules
/// 4. Inline `style` prop
pub fn resolve_style(node: &Node, stylesheet: &Stylesheet) -> StyleDecl {
    let tag = dom_tag(node);
    let component = component_name(node);
    let class_names: Vec<&str> = node
        .class_name
        .as_deref()
        .map(|s| s.split_whitespace().collect())
        .unwrap_or_default();

    let rule_matches = |rule: &crate::css_parser::CssRule| -> bool {
        rule.selectors
            .iter()
            .any(|s| s.matches(tag, &class_names) || s.matches(component, &class_names))
    };

    let mut merged = StyleDecl::default();

    // 1. Apply layered rules in layer declaration order (earlier = lower priority)
    for layer_name in &stylesheet.layer_order {
        for rule in &stylesheet.rules {
            if rule.layer.as_deref() == Some(layer_name) && rule_matches(rule) {
                merge_style(&mut merged, &rule.style);
            }
        }
    }

    // 2. Apply un-layered CSS rules (higher than any layer)
    for rule in &stylesheet.rules {
        if rule.layer.is_none() && rule_matches(rule) {
            merge_style(&mut merged, &rule.style);
        }
    }

    // 3. Inline styles override all CSS
    merge_style(&mut merged, &node.style);

    merged
}

pub fn merge_style(base: &mut StyleDecl, over: &StyleDecl) {
    macro_rules! merge {
        ($($field:ident),+ $(,)?) => {
            $(if over.$field.is_some() { base.$field = over.$field.clone(); })+
        };
    }
    merge!(
        gap,
        padding,
        margin,
        font_size,
        font_weight,
        font_family,
        font_style,
        color,
        background,
        border_radius,
        border_width,
        border_color,
        align_items,
        align_self,
        align_content,
        justify_content,
        width,
        height,
        min_width,
        min_height,
        max_width,
        max_height,
        flex_grow,
        flex_shrink,
        flex_basis,
        flex_direction,
        flex_wrap,
        order,
        position,
        top,
        right,
        bottom,
        left,
        z_index,
        overflow,
        display,
        opacity,
        visibility,
        cursor,
        pointer_events,
        user_select,
        text_align,
        white_space,
        line_height,
        letter_spacing,
        text_decoration,
        text_overflow,
        word_break,
        outline_width,
        outline_color,
        outline_style,
        transform,
        transition,
        box_shadow,
        custom_properties,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css_parser::parse_css;

    fn make_node(kind: NodeKind, class_name: Option<&str>, style: StyleDecl) -> Node {
        Node {
            kind,
            style,
            children: vec![],
            text: None,
            label: None,
            on_click: None,
            src: None,
            placeholder: None,
            class_name: class_name.map(|s| s.to_string()),
        }
    }

    #[test]
    fn resolve_class_match() {
        let css = ".title { font-size: 24; color: #e94560; }";
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Text("Hello".into()),
            Some("title"),
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.font_size, Some(24.0));
        assert_eq!(resolved.color.as_deref(), Some("#e94560"));
    }

    #[test]
    fn inline_overrides_css() {
        let css = ".title { color: red; font-size: 24; }";
        let sheet = parse_css(css);
        let mut style = StyleDecl::default();
        style.color = Some("#fff".to_string());
        let node = make_node(NodeKind::Text("Hello".into()), Some("title"), style);
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.color.as_deref(), Some("#fff"));
        assert_eq!(resolved.font_size, Some(24.0));
    }

    #[test]
    fn no_match_returns_inline() {
        let css = ".other { color: red; }";
        let sheet = parse_css(css);
        let mut style = StyleDecl::default();
        style.font_size = Some(16.0);
        let node = make_node(NodeKind::Text("Hello".into()), Some("title"), style);
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.color, None);
        assert_eq!(resolved.font_size, Some(16.0));
    }

    // DOM tag selectors — standard W3C CSS element selectors

    #[test]
    fn dom_span_matches_text() {
        let css = "span { font-size: 14; }";
        let sheet = parse_css(css);
        let node = make_node(NodeKind::Text("Hi".into()), None, StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.font_size, Some(14.0));
    }

    #[test]
    fn dom_div_matches_column() {
        let css = "div { padding: 10; gap: 8; }";
        let sheet = parse_css(css);
        let node = make_node(NodeKind::Column, None, StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.padding, Some(10.0));
        assert_eq!(resolved.gap, Some(8.0));
    }

    #[test]
    fn dom_div_matches_row() {
        let css = "div { gap: 12; }";
        let sheet = parse_css(css);
        let node = make_node(NodeKind::Row, None, StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.gap, Some(12.0));
    }

    #[test]
    fn dom_div_matches_box() {
        let css = "div { padding: 8; }";
        let sheet = parse_css(css);
        let node = make_node(NodeKind::Box, None, StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.padding, Some(8.0));
    }

    #[test]
    fn dom_button_matches_button() {
        let css = "button { background: #e94560; border-radius: 8; }";
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Button("Go".into()),
            None,
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.background.as_deref(), Some("#e94560"));
        assert_eq!(resolved.border_radius, Some(8.0));
    }

    #[test]
    fn dom_img_matches_image() {
        let css = "img { width: 100px; border-radius: 4; }";
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Image("logo.png".into()),
            None,
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.width.as_deref(), Some("100px"));
        assert_eq!(resolved.border_radius, Some(4.0));
    }

    #[test]
    fn dom_input_matches_textinput() {
        let css = "input { padding: 12; border-width: 1; }";
        let sheet = parse_css(css);
        let node = make_node(NodeKind::TextInput, None, StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.padding, Some(12.0));
        assert_eq!(resolved.border_width, Some(1.0));
    }

    #[test]
    fn dom_compound_selector() {
        let css = "span.title { font-size: 32; color: #fff; }";
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Text("Hello".into()),
            Some("title"),
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.font_size, Some(32.0));
        assert_eq!(resolved.color.as_deref(), Some("#fff"));
    }

    #[test]
    fn dom_compound_no_match_wrong_tag() {
        let css = "button.title { font-size: 32; }";
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Text("Hello".into()),
            Some("title"),
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.font_size, None);
    }

    // Component name aliases still work

    #[test]
    fn component_name_alias_text() {
        let css = "Text { font-size: 14; }";
        let sheet = parse_css(css);
        let node = make_node(NodeKind::Text("Hi".into()), None, StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.font_size, Some(14.0));
    }

    #[test]
    fn component_name_alias_column() {
        let css = "Column { gap: 20; }";
        let sheet = parse_css(css);
        let node = make_node(NodeKind::Column, None, StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.gap, Some(20.0));
    }

    // Precedence and merging

    #[test]
    fn dom_rules_merge_with_class() {
        let css = r#"
            span { font-size: 14; }
            .title { font-size: 24; color: #fff; }
        "#;
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Text("Hello".into()),
            Some("title"),
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.font_size, Some(24.0));
        assert_eq!(resolved.color.as_deref(), Some("#fff"));
    }

    #[test]
    fn multi_class_node() {
        let css = r#"
            .primary { background: #e94560; }
            .large { font-size: 32; }
        "#;
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Button("Go".into()),
            Some("primary large"),
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        assert_eq!(resolved.background.as_deref(), Some("#e94560"));
        assert_eq!(resolved.font_size, Some(32.0));
    }

    // ── @layer cascade tests ──

    #[test]
    fn layer_later_overrides_earlier() {
        let css = r#"
            @layer reset, theme;

            @layer reset {
                .title { color: black; font-size: 16; }
            }
            @layer theme {
                .title { color: #e94560; }
            }
        "#;
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Text("Hi".into()),
            Some("title"),
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        // theme overrides reset
        assert_eq!(resolved.color.as_deref(), Some("#e94560"));
        // font-size from reset still applies (not overridden by theme)
        assert_eq!(resolved.font_size, Some(16.0));
    }

    #[test]
    fn unlayered_overrides_all_layers() {
        let css = r#"
            @layer base {
                .title { color: blue; font-size: 24; }
            }
            @layer theme {
                .title { color: green; }
            }
            .title { color: red; }
        "#;
        let sheet = parse_css(css);
        let node = make_node(
            NodeKind::Text("Hi".into()),
            Some("title"),
            StyleDecl::default(),
        );
        let resolved = resolve_style(&node, &sheet);
        // Un-layered red overrides both layers
        assert_eq!(resolved.color.as_deref(), Some("red"));
        // font-size from base layer still applies
        assert_eq!(resolved.font_size, Some(24.0));
    }

    #[test]
    fn inline_overrides_unlayered_and_layers() {
        let css = r#"
            @layer base {
                .title { color: blue; font-size: 20; }
            }
            .title { color: green; font-weight: bold; }
        "#;
        let sheet = parse_css(css);
        let mut style = StyleDecl::default();
        style.color = Some("#fff".into());
        let node = make_node(NodeKind::Text("Hi".into()), Some("title"), style);
        let resolved = resolve_style(&node, &sheet);
        // Inline #fff overrides un-layered green and layered blue
        assert_eq!(resolved.color.as_deref(), Some("#fff"));
        // font-size from layer, font-weight from un-layered
        assert_eq!(resolved.font_size, Some(20.0));
        assert_eq!(resolved.font_weight, Some(700));
    }

    #[test]
    fn layer_three_tiers() {
        let css = r#"
            @layer reset, base, components;

            @layer reset {
                .card { padding: 0; gap: 0; background: transparent; }
            }
            @layer base {
                .card { padding: 16; gap: 8; }
            }
            @layer components {
                .card { padding: 24; }
            }
        "#;
        let sheet = parse_css(css);
        let node = make_node(NodeKind::Column, Some("card"), StyleDecl::default());
        let resolved = resolve_style(&node, &sheet);
        // components (last layer) wins for padding
        assert_eq!(resolved.padding, Some(24.0));
        // base wins for gap (components didn't set it)
        assert_eq!(resolved.gap, Some(8.0));
        // reset's background survives (not overridden)
        assert_eq!(resolved.background.as_deref(), Some("transparent"));
    }
}
