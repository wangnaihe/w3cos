use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppTree {
    pub root: Node,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub kind: NodeKind,
    #[serde(default)]
    pub style: StyleDecl,
    #[serde(default)]
    pub children: Vec<Node>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeKind {
    Column,
    Row,
    #[serde(alias = "text")]
    Text(#[serde(default)] String),
    #[serde(alias = "button")]
    Button(#[serde(default)] String),
    Box,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StyleDecl {
    pub gap: Option<f32>,
    pub padding: Option<f32>,
    pub font_size: Option<f32>,
    pub font_weight: Option<u16>,
    pub color: Option<String>,
    pub background: Option<String>,
    pub border_radius: Option<f32>,
    pub border_width: Option<f32>,
    pub border_color: Option<String>,
    pub align_items: Option<String>,
    pub justify_content: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
    pub flex_grow: Option<f32>,
    pub position: Option<String>,
    pub top: Option<String>,
    pub right: Option<String>,
    pub bottom: Option<String>,
    pub left: Option<String>,
    pub z_index: Option<i32>,
    pub overflow: Option<String>,
    pub display: Option<String>,
}

pub fn parse(source: &str) -> Result<AppTree> {
    let trimmed = source.trim();

    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        parse_json(trimmed)
    } else {
        parse_ts(trimmed)
    }
}

fn parse_json(source: &str) -> Result<AppTree> {
    let mut root: Node =
        serde_json::from_str(source).map_err(|e| anyhow!("JSON parse error: {e}"))?;
    fixup_json_node(&mut root);
    Ok(AppTree { root })
}

fn fixup_json_node(node: &mut Node) {
    if let Some(ref text) = node.text
        && matches!(node.kind, NodeKind::Text(_))
    {
        node.kind = NodeKind::Text(text.clone());
    }
    if let Some(ref label) = node.label
        && matches!(node.kind, NodeKind::Button(_))
    {
        node.kind = NodeKind::Button(label.clone());
    }
    for child in &mut node.children {
        fixup_json_node(child);
    }
}

// ---------------------------------------------------------------------------
// TypeScript Subset Parser
// Supports: Column({...}), Row({...}), Text("...", {...}), Button("...", {...})
// ---------------------------------------------------------------------------

fn parse_ts(source: &str) -> Result<AppTree> {
    let clean = strip_ts_wrapper(source);
    let clean = clean.trim();
    if clean.is_empty() {
        return Ok(AppTree {
            root: empty_column(),
        });
    }
    parse_ts_expr(clean)
        .map(|root| AppTree { root })
        .ok_or_else(|| {
            anyhow!(
                "Failed to parse. Supported syntax:\n\
            Function: Column({{ style: {{...}}, children: [...] }})\n\
            TSX:      <Column style={{{{ gap: 20 }}}}><Text>Hello</Text></Column>"
            )
        })
}

fn strip_ts_wrapper(source: &str) -> String {
    let mut lines: Vec<&str> = source.lines().collect();

    // Remove import lines
    lines.retain(|l| {
        let t = l.trim();
        !t.starts_with("import ")
    });

    let mut result = lines.join("\n");

    // Remove `export default` prefix
    if let Some(rest) = result.trim().strip_prefix("export default") {
        result = rest.trim().to_string();
    }

    // Remove trailing semicolons
    if result.trim().ends_with(';') {
        result = result.trim().trim_end_matches(';').to_string();
    }

    result
}

fn parse_ts_expr(s: &str) -> Option<Node> {
    let s = s.trim();

    // TSX: <Column ...>...</Column> or <Text .../>
    if s.starts_with('<') {
        return parse_tsx_element(s).map(|(node, _)| node);
    }

    // Column({ ... })
    if let Some(inner) = strip_fn_call(s, "Column") {
        return parse_container_call("Column", inner);
    }
    // Row({ ... })
    if let Some(inner) = strip_fn_call(s, "Row") {
        return parse_container_call("Row", inner);
    }
    // Text("content", { style: {...} })
    if let Some(inner) = strip_fn_call(s, "Text") {
        return parse_text_or_button(inner, true);
    }
    // Button("label", { style: {...} })
    if let Some(inner) = strip_fn_call(s, "Button") {
        return parse_text_or_button(inner, false);
    }

    None
}

// ---------------------------------------------------------------------------
// TSX Parser
// Supports: <Column>, <Row>, <Box>, <Text>, <Button> with style={{...}} props
// ---------------------------------------------------------------------------

fn parse_tsx_element(s: &str) -> Option<(Node, &str)> {
    let s = s.trim();
    if !s.starts_with('<') {
        return None;
    }

    let after_lt = &s[1..];
    if after_lt.starts_with('/') {
        return None;
    }

    let tag_end = after_lt.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
    let tag_name = &after_lt[..tag_end];

    if !matches!(tag_name, "Column" | "Row" | "Box" | "Text" | "Button") {
        return None;
    }

    let after_tag = &after_lt[tag_end..];
    let (style, after_attrs) = parse_tsx_attrs(after_tag);
    let after_attrs = after_attrs.trim();

    if let Some(rest) = after_attrs.strip_prefix("/>") {
        let node = build_tsx_node(tag_name, style, vec![], None);
        return Some((node, rest));
    }

    if !after_attrs.starts_with('>') {
        return None;
    }
    let after_open = &after_attrs[1..];

    let (children, text_content, after_children) = parse_tsx_children(after_open, tag_name)?;

    let node = build_tsx_node(tag_name, style, children, text_content);
    Some((node, after_children))
}

fn parse_tsx_attrs(s: &str) -> (StyleDecl, &str) {
    let mut style = StyleDecl::default();
    let mut rest = s.trim();

    while !rest.is_empty() && !rest.starts_with('>') && !rest.starts_with("/>") {
        rest = rest.trim();
        if let Some(after) = rest.strip_prefix("style=") {
            let after = after.trim();
            if let Some(inner) = after.strip_prefix("{{")
                && let Some(end) = find_double_brace_end(inner)
            {
                let style_str = &inner[..end];
                if let Some(parsed) = parse_style_object(&format!("{{{style_str}}}")) {
                    style = parsed;
                }
                rest = &inner[end + 2..]; // skip }}
                continue;
            }
        }
        // Skip unknown attributes
        if let Some(eq_pos) = rest.find('=') {
            let after_eq = rest[eq_pos + 1..].trim();
            if let Some(inner) = after_eq.strip_prefix("{{") {
                if let Some(end) = find_double_brace_end(inner) {
                    rest = &inner[end + 2..];
                    continue;
                }
            } else if after_eq.starts_with('{') {
                if let Some(brace) = find_matching_brace(after_eq) {
                    rest = &after_eq[brace.len()..];
                    continue;
                }
            } else if let Some((_, r)) = extract_first_string_arg(after_eq) {
                rest = r;
                continue;
            }
        }
        // Advance past current char to avoid infinite loop
        if let Some(next) = rest.find(['>', '/']) {
            rest = &rest[next..];
        } else {
            break;
        }
    }

    (style, rest)
}

fn find_double_brace_end(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_tsx_children<'a>(
    s: &'a str,
    parent_tag: &str,
) -> Option<(Vec<Node>, Option<String>, &'a str)> {
    let closing = format!("</{parent_tag}>");
    let mut children = Vec::new();
    let mut text_buf = String::new();
    let mut rest = s;

    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }

        if let Some(after_close) = rest.strip_prefix(closing.as_str()) {
            let text = text_buf.trim().to_string();
            let text_opt = if text.is_empty() { None } else { Some(text) };
            return Some((children, text_opt, after_close));
        }

        if rest.starts_with("</") {
            return None;
        }

        if rest.starts_with('<')
            && let Some((child, after)) = parse_tsx_element(rest)
        {
            children.push(child);
            rest = after;
            continue;
        }

        // Text content — collect until next '<'
        if let Some(lt_pos) = rest.find('<') {
            let text_part = &rest[..lt_pos];
            if !text_part.trim().is_empty() {
                if !text_buf.is_empty() {
                    text_buf.push(' ');
                }
                text_buf.push_str(text_part.trim());
            }
            rest = &rest[lt_pos..];
        } else {
            text_buf.push_str(rest.trim());
            rest = "";
        }
    }
}

fn build_tsx_node(
    tag: &str,
    style: StyleDecl,
    children: Vec<Node>,
    text_content: Option<String>,
) -> Node {
    match tag {
        "Text" => {
            let content = text_content.unwrap_or_default();
            Node {
                kind: NodeKind::Text(content.clone()),
                style,
                children: vec![],
                text: Some(content),
                label: None,
            }
        }
        "Button" => {
            let label = text_content.unwrap_or_default();
            Node {
                kind: NodeKind::Button(label.clone()),
                style,
                children: vec![],
                text: None,
                label: Some(label),
            }
        }
        "Row" => Node {
            kind: NodeKind::Row,
            style,
            children,
            text: None,
            label: None,
        },
        "Box" => Node {
            kind: NodeKind::Box,
            style,
            children,
            text: None,
            label: None,
        },
        _ => Node {
            kind: NodeKind::Column,
            style,
            children,
            text: None,
            label: None,
        },
    }
}

fn strip_fn_call<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let s = s.trim();
    if !s.starts_with(name) {
        return None;
    }
    let rest = s[name.len()..].trim();
    if !rest.starts_with('(') || !rest.ends_with(')') {
        return None;
    }
    Some(&rest[1..rest.len() - 1])
}

fn parse_container_call(kind: &str, inner: &str) -> Option<Node> {
    let inner = inner.trim();

    // Expect: { style: {...}, children: [...] }
    // or: { children: [...] }
    // or: { style: {...} }
    let style = extract_object_field(inner, "style")
        .and_then(parse_style_object)
        .unwrap_or_default();

    let children = extract_array_field(inner, "children")
        .map(parse_children_array)
        .unwrap_or_default();

    Some(Node {
        kind: match kind {
            "Row" => NodeKind::Row,
            _ => NodeKind::Column,
        },
        style,
        children,
        text: None,
        label: None,
    })
}

fn parse_text_or_button(inner: &str, is_text: bool) -> Option<Node> {
    let inner = inner.trim();

    // First arg: string literal
    let (content, rest) = extract_first_string_arg(inner)?;

    // Optional second arg: { style: {...} }
    let style = if let Some(rest) = rest.strip_prefix(',') {
        let rest = rest.trim();
        if rest.starts_with('{') {
            let obj = find_matching_brace(rest)?;
            extract_object_field(obj, "style")
                .and_then(parse_style_object)
                .unwrap_or_default()
        } else {
            StyleDecl::default()
        }
    } else {
        StyleDecl::default()
    };

    Some(Node {
        kind: if is_text {
            NodeKind::Text(content.clone())
        } else {
            NodeKind::Button(content.clone())
        },
        style,
        children: vec![],
        text: if is_text { Some(content.clone()) } else { None },
        label: if !is_text { Some(content) } else { None },
    })
}

fn extract_first_string_arg(s: &str) -> Option<(String, &str)> {
    let s = s.trim();
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' && quote != '`' {
        return None;
    }
    let end = s[1..].find(quote)?;
    let content = s[1..1 + end].to_string();
    let rest = s[2 + end..].trim();
    Some((content, rest))
}

fn extract_object_field<'a>(obj: &'a str, field: &str) -> Option<&'a str> {
    // Find `field:` or `field :` in the object
    let search = format!("{}:", field);
    let search_spaced = format!("{} :", field);

    let pos = obj.find(&search).or_else(|| obj.find(&search_spaced))?;

    let after_colon = &obj[pos + field.len()..];
    let after_colon = after_colon.trim_start_matches(|c: char| c == ':' || c.is_whitespace());

    if after_colon.starts_with('{') {
        find_matching_brace(after_colon)
    } else if after_colon.starts_with('[') {
        find_matching_bracket(after_colon)
    } else {
        // Simple value until comma or closing brace
        let end = after_colon
            .find(|c| [',', '}'].contains(&c))
            .unwrap_or(after_colon.len());
        Some(after_colon[..end].trim())
    }
}

fn extract_array_field<'a>(obj: &'a str, field: &str) -> Option<&'a str> {
    let search = format!("{}:", field);
    let search_spaced = format!("{} :", field);

    let pos = obj.find(&search).or_else(|| obj.find(&search_spaced))?;

    let after_colon = &obj[pos + field.len()..];
    let after_colon = after_colon.trim_start_matches(|c: char| c == ':' || c.is_whitespace());

    if after_colon.starts_with('[') {
        find_matching_bracket(after_colon)
    } else {
        None
    }
}

fn find_matching_brace(s: &str) -> Option<&str> {
    if !s.starts_with('{') {
        return None;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_matching_bracket(s: &str) -> Option<&str> {
    if !s.starts_with('[') {
        return None;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_children_array(arr: &str) -> Vec<Node> {
    let inner = arr.trim();
    let inner = &inner[1..inner.len() - 1]; // strip [ ]

    let mut children = Vec::new();
    let mut depth_brace = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut start = 0;

    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '[' => depth_bracket += 1,
            ']' => depth_bracket -= 1,
            ',' if depth_brace == 0 && depth_paren == 0 && depth_bracket == 0 => {
                let item = inner[start..i].trim();
                if !item.is_empty()
                    && let Some(node) = parse_ts_expr(item)
                {
                    children.push(node);
                }
                start = i + 1;
            }
            _ => {}
        }
    }

    let last = inner[start..].trim();
    if !last.is_empty()
        && let Some(node) = parse_ts_expr(last)
    {
        children.push(node);
    }

    children
}

fn parse_style_object(obj: &str) -> Option<StyleDecl> {
    let inner = obj.trim();
    let inner = if inner.starts_with('{') && inner.ends_with('}') {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };

    let mut style = StyleDecl::default();

    for pair in split_top_level(inner, ',') {
        let pair = pair.trim();
        if let Some(colon_pos) = pair.find(':') {
            let key = pair[..colon_pos]
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            let val = pair[colon_pos + 1..].trim();

            match key {
                "gap" => style.gap = val.parse().ok(),
                "padding" => style.padding = val.parse().ok(),
                "fontSize" | "font_size" => style.font_size = val.parse().ok(),
                "fontWeight" | "font_weight" => style.font_weight = val.parse().ok(),
                "color" => style.color = Some(unquote(val)),
                "background" => style.background = Some(unquote(val)),
                "borderRadius" | "border_radius" => style.border_radius = val.parse().ok(),
                "borderWidth" | "border_width" => style.border_width = val.parse().ok(),
                "borderColor" | "border_color" => style.border_color = Some(unquote(val)),
                "alignItems" | "align_items" => style.align_items = Some(unquote(val)),
                "justifyContent" | "justify_content" => style.justify_content = Some(unquote(val)),
                "flexGrow" | "flex_grow" => style.flex_grow = val.parse().ok(),
                "position" => style.position = Some(unquote(val)),
                "top" => style.top = Some(unquote(val)),
                "right" => style.right = Some(unquote(val)),
                "bottom" => style.bottom = Some(unquote(val)),
                "left" => style.left = Some(unquote(val)),
                "zIndex" | "z_index" => style.z_index = val.parse().ok(),
                "width" => style.width = Some(unquote(val)),
                "height" => style.height = Some(unquote(val)),
                "overflow" => style.overflow = Some(unquote(val)),
                "display" => style.display = Some(unquote(val)),
                _ => {}
            }
        }
    }

    Some(style)
}

fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut results = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                results.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = &s[start..];
    if !last.trim().is_empty() {
        results.push(last);
    }
    results
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"'))
        || (s.starts_with('\'') && s.ends_with('\''))
        || (s.starts_with('`') && s.ends_with('`'))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn empty_column() -> Node {
    Node {
        kind: NodeKind::Column,
        style: StyleDecl::default(),
        children: vec![],
        text: None,
        label: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_with_style() {
        let source = r##"Text("hello", { style: { color: "#fff" } })"##;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Text(content) => {
                assert_eq!(content, "hello");
                assert_eq!(tree.root.text.as_deref(), Some("hello"));
            }
            _ => panic!("expected Text node"),
        }
        assert_eq!(tree.root.style.color.as_deref(), Some("#fff"));
    }

    #[test]
    fn parse_column_with_children() {
        let source = r#"Column({ children: [Text("a"), Text("b")] })"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected Column node"),
        }
        assert_eq!(tree.root.children.len(), 2);
        match &tree.root.children[0].kind {
            NodeKind::Text(a) => assert_eq!(a, "a"),
            _ => panic!("expected Text child"),
        }
        match &tree.root.children[1].kind {
            NodeKind::Text(b) => assert_eq!(b, "b"),
            _ => panic!("expected Text child"),
        }
    }

    #[test]
    fn parse_button_with_style() {
        let source = r##"Button("click me", { style: { background: "#e94560" } })"##;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Button(label) => assert_eq!(label, "click me"),
            _ => panic!("expected Button node"),
        }
        assert_eq!(tree.root.style.background.as_deref(), Some("#e94560"));
    }

    #[test]
    fn parse_row_with_style_and_children() {
        let source = r#"Row({ style: { gap: 10 }, children: [Text("x"), Button("ok")] })"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Row => {}
            _ => panic!("expected Row node"),
        }
        assert_eq!(tree.root.style.gap, Some(10.0));
        assert_eq!(tree.root.children.len(), 2);
        match &tree.root.children[0].kind {
            NodeKind::Text(x) => assert_eq!(x, "x"),
            _ => panic!("expected Text child"),
        }
        match &tree.root.children[1].kind {
            NodeKind::Button(ok) => assert_eq!(ok, "ok"),
            _ => panic!("expected Button child"),
        }
    }

    #[test]
    fn parse_json_format() {
        // Serde expects: unit variants as {"Column": null}, newtype as {"Text": "content"}
        let source = r#"{
            "kind": {"Column": null},
            "children": [
                { "kind": {"Text": ""}, "text": "hello" },
                { "kind": {"Button": ""}, "label": "submit" }
            ]
        }"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected Column node from JSON"),
        }
        assert_eq!(tree.root.children.len(), 2);
        match &tree.root.children[0].kind {
            NodeKind::Text(h) => assert_eq!(h, "hello"),
            _ => panic!("expected Text child"),
        }
        match &tree.root.children[1].kind {
            NodeKind::Button(s) => assert_eq!(s, "submit"),
            _ => panic!("expected Button child"),
        }
    }

    #[test]
    fn parse_json_with_style() {
        let source = r##"{
            "kind": {"Text": "styled"},
            "text": "styled",
            "style": { "color": "#ff0000", "font_size": 16 }
        }"##;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Text(t) => assert_eq!(t, "styled"),
            _ => panic!("expected Text node"),
        }
        assert_eq!(tree.root.style.color.as_deref(), Some("#ff0000"));
        assert_eq!(tree.root.style.font_size, Some(16.0));
    }

    #[test]
    fn parse_export_default_wrapper() {
        let source = r#"export default Column({ children: [Text("wrapped")] })"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected Column node"),
        }
        assert_eq!(tree.root.children.len(), 1);
        match &tree.root.children[0].kind {
            NodeKind::Text(w) => assert_eq!(w, "wrapped"),
            _ => panic!("expected Text child"),
        }
    }

    #[test]
    fn parse_empty_returns_column() {
        let tree = parse("").unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected empty Column"),
        }
        assert!(tree.root.children.is_empty());
    }

    // TSX tests

    #[test]
    fn tsx_simple_text() {
        let source = r#"<Text>Hello</Text>"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Text(t) => assert_eq!(t, "Hello"),
            _ => panic!("expected Text, got {:?}", tree.root.kind),
        }
    }

    #[test]
    fn tsx_button() {
        let source = r#"<Button>Click me</Button>"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Button(l) => assert_eq!(l, "Click me"),
            _ => panic!("expected Button"),
        }
        assert_eq!(tree.root.label.as_deref(), Some("Click me"));
    }

    #[test]
    fn tsx_text_with_style() {
        let source = r##"<Text style={{ fontSize: 42, color: "#e94560" }}>W3C OS</Text>"##;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Text(t) => assert_eq!(t, "W3C OS"),
            _ => panic!("expected Text"),
        }
        assert_eq!(tree.root.style.font_size, Some(42.0));
        assert_eq!(tree.root.style.color.as_deref(), Some("#e94560"));
    }

    #[test]
    fn tsx_column_with_children() {
        let source = r#"<Column>
            <Text>a</Text>
            <Text>b</Text>
        </Column>"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected Column"),
        }
        assert_eq!(tree.root.children.len(), 2);
        match &tree.root.children[0].kind {
            NodeKind::Text(a) => assert_eq!(a, "a"),
            _ => panic!("expected Text child"),
        }
    }

    #[test]
    fn tsx_nested_layout() {
        let source = r##"<Column style={{ gap: 20, background: "#0f0f1a" }}>
            <Text style={{ fontSize: 42 }}>Title</Text>
            <Row style={{ gap: 16 }}>
                <Button style={{ background: "#e94560" }}>Go</Button>
                <Button>Cancel</Button>
            </Row>
        </Column>"##;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected Column"),
        }
        assert_eq!(tree.root.style.gap, Some(20.0));
        assert_eq!(tree.root.children.len(), 2);

        let row = &tree.root.children[1];
        match &row.kind {
            NodeKind::Row => {}
            _ => panic!("expected Row"),
        }
        assert_eq!(row.style.gap, Some(16.0));
        assert_eq!(row.children.len(), 2);

        match &row.children[0].kind {
            NodeKind::Button(l) => assert_eq!(l, "Go"),
            _ => panic!("expected Button"),
        }
        assert_eq!(row.children[0].style.background.as_deref(), Some("#e94560"));
    }

    #[test]
    fn tsx_self_closing() {
        let source = r#"<Column style={{ gap: 10 }} />"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected Column"),
        }
        assert_eq!(tree.root.style.gap, Some(10.0));
        assert!(tree.root.children.is_empty());
    }

    #[test]
    fn tsx_with_export_default() {
        let source = r#"
import { Column, Text } from "@w3cos/std"

export default <Column>
    <Text>Hello TSX</Text>
</Column>
"#;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Column => {}
            _ => panic!("expected Column"),
        }
        assert_eq!(tree.root.children.len(), 1);
        match &tree.root.children[0].kind {
            NodeKind::Text(t) => assert_eq!(t, "Hello TSX"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn tsx_row_with_box() {
        let source = r##"<Row style={{ padding: 16 }}>
            <Box style={{ flexGrow: 1, background: "#1e1e2e" }}>
                <Text>Content</Text>
            </Box>
        </Row>"##;
        let tree = parse(source).unwrap();
        match &tree.root.kind {
            NodeKind::Row => {}
            _ => panic!("expected Row"),
        }
        assert_eq!(tree.root.children.len(), 1);
        match &tree.root.children[0].kind {
            NodeKind::Box => {}
            _ => panic!("expected Box"),
        }
        assert_eq!(tree.root.children[0].style.flex_grow, Some(1.0));
    }
}
