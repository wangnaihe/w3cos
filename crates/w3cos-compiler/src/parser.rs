use anyhow::{anyhow, Result};
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
    let mut root: Node = serde_json::from_str(source)
        .map_err(|e| anyhow!("JSON parse error: {e}"))?;
    fixup_json_node(&mut root);
    Ok(AppTree { root })
}

fn fixup_json_node(node: &mut Node) {
    if let Some(ref text) = node.text {
        if matches!(node.kind, NodeKind::Text(_)) {
            node.kind = NodeKind::Text(text.clone());
        }
    }
    if let Some(ref label) = node.label {
        if matches!(node.kind, NodeKind::Button(_)) {
            node.kind = NodeKind::Button(label.clone());
        }
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
        return Ok(AppTree { root: empty_column() });
    }
    parse_ts_expr(clean)
        .map(|root| AppTree { root })
        .ok_or_else(|| anyhow!("Failed to parse TypeScript. Supported syntax:\n\
            Column({{ style: {{...}}, children: [...] }})\n\
            Text(\"content\", {{ style: {{...}} }})\n\
            Button(\"label\", {{ style: {{...}} }})"))
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
        .and_then(|s| parse_style_object(s))
        .unwrap_or_default();

    let children = extract_array_field(inner, "children")
        .map(|arr| parse_children_array(arr))
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
                .and_then(|s| parse_style_object(s))
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

    let pos = obj.find(&search)
        .or_else(|| obj.find(&search_spaced))?;

    let after_colon = &obj[pos + field.len()..];
    let after_colon = after_colon.trim_start_matches(|c: char| c == ':' || c.is_whitespace());

    if after_colon.starts_with('{') {
        find_matching_brace(after_colon)
    } else if after_colon.starts_with('[') {
        find_matching_bracket(after_colon)
    } else {
        // Simple value until comma or closing brace
        let end = after_colon.find(|c: char| c == ',' || c == '}').unwrap_or(after_colon.len());
        Some(after_colon[..end].trim())
    }
}

fn extract_array_field<'a>(obj: &'a str, field: &str) -> Option<&'a str> {
    let search = format!("{}:", field);
    let search_spaced = format!("{} :", field);

    let pos = obj.find(&search)
        .or_else(|| obj.find(&search_spaced))?;

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
                if !item.is_empty() {
                    if let Some(node) = parse_ts_expr(item) {
                        children.push(node);
                    }
                }
                start = i + 1;
            }
            _ => {}
        }
    }

    let last = inner[start..].trim();
    if !last.is_empty() {
        if let Some(node) = parse_ts_expr(last) {
            children.push(node);
        }
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
            let key = pair[..colon_pos].trim().trim_matches('"').trim_matches('\'');
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
