use std::collections::HashMap;

use crate::parser::StyleDecl;

#[derive(Debug, Clone)]
pub struct KeyframeStop {
    pub offset: f32,
    pub style: StyleDecl,
}

#[derive(Debug, Clone)]
pub struct KeyframeAnimation {
    pub name: String,
    pub stops: Vec<KeyframeStop>,
}

#[derive(Debug, Clone)]
pub struct FontFace {
    pub family: String,
    pub src: String,
    pub weight: Option<String>,
    pub style: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Stylesheet {
    pub layer_order: Vec<String>,
    pub rules: Vec<CssRule>,
    pub keyframes: Vec<KeyframeAnimation>,
    pub font_faces: Vec<FontFace>,
}

impl Stylesheet {
    pub fn empty() -> Self {
        Self {
            layer_order: Vec::new(),
            rules: Vec::new(),
            keyframes: Vec::new(),
            font_faces: Vec::new(),
        }
    }

    pub fn merge(&mut self, other: Stylesheet) {
        for layer in other.layer_order {
            if !self.layer_order.contains(&layer) {
                self.layer_order.push(layer);
            }
        }
        self.rules.extend(other.rules);
        self.keyframes.extend(other.keyframes);
        self.font_faces.extend(other.font_faces);
    }
}

#[derive(Debug, Clone)]
pub struct CssRule {
    pub selectors: Vec<Selector>,
    pub style: StyleDecl,
    pub layer: Option<String>,
}

/// CSS pseudo-class attached to a selector.
#[derive(Debug, Clone, PartialEq)]
pub enum PseudoClass {
    Hover,
    Focus,
    Active,
    Visited,
    Disabled,
    Enabled,
    Checked,
    FirstChild,
    LastChild,
    NthChild(NthExpr),
    NthLastChild(NthExpr),
    OnlyChild,
    Empty,
    Not(Box<Selector>),
}

/// `An+B` expression for `:nth-child()`.
#[derive(Debug, Clone, PartialEq)]
pub struct NthExpr {
    pub a: i32,
    pub b: i32,
}

impl NthExpr {
    pub fn matches(&self, index_1based: usize) -> bool {
        let n = index_1based as i32;
        if self.a == 0 {
            return n == self.b;
        }
        let diff = n - self.b;
        diff % self.a == 0 && diff / self.a >= 0
    }
}

/// CSS attribute selector operator.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrOp {
    Exists,
    Exact(String),
    Contains(String),
    StartsWith(String),
    EndsWith(String),
    DashMatch(String),
    Includes(String),
}

/// A single CSS attribute selector: `[attr]`, `[attr=value]`, etc.
#[derive(Debug, Clone, PartialEq)]
pub struct AttrSelector {
    pub name: String,
    pub op: AttrOp,
}

impl AttrSelector {
    pub fn matches_value(&self, value: Option<&str>) -> bool {
        match &self.op {
            AttrOp::Exists => value.is_some(),
            AttrOp::Exact(expected) => value == Some(expected.as_str()),
            AttrOp::Contains(sub) => value.is_some_and(|v| v.contains(sub.as_str())),
            AttrOp::StartsWith(prefix) => value.is_some_and(|v| v.starts_with(prefix.as_str())),
            AttrOp::EndsWith(suffix) => value.is_some_and(|v| v.ends_with(suffix.as_str())),
            AttrOp::DashMatch(val) => value.is_some_and(|v| {
                v == val || v.starts_with(&format!("{val}-"))
            }),
            AttrOp::Includes(word) => value.is_some_and(|v| {
                v.split_whitespace().any(|w| w == word)
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Selector {
    Universal,
    Element(String),
    Class(String),
    Compound {
        element: Option<String>,
        classes: Vec<String>,
        pseudo_classes: Vec<PseudoClass>,
        attrs: Vec<AttrSelector>,
    },
}

/// Context information for structural pseudo-class matching.
pub struct MatchContext<'a> {
    pub element_kind: &'a str,
    pub class_names: &'a [&'a str],
    pub attributes: &'a [(&'a str, &'a str)],
    pub child_index: usize,
    pub sibling_count: usize,
    pub child_count: usize,
    pub pseudo_state: PseudoState,
}

/// Runtime pseudo-class state (hover, focus, active).
#[derive(Debug, Clone, Copy, Default)]
pub struct PseudoState {
    pub hovered: bool,
    pub focused: bool,
    pub active: bool,
    pub checked: bool,
    pub disabled: bool,
}

impl Selector {
    pub fn matches(&self, element_kind: &str, class_names: &[&str]) -> bool {
        match self {
            Selector::Universal => true,
            Selector::Element(e) => e == element_kind,
            Selector::Class(c) => class_names.contains(&c.as_str()),
            Selector::Compound { element, classes, pseudo_classes, attrs } => {
                if let Some(e) = element
                    && e != element_kind
                {
                    return false;
                }
                if !classes.iter().all(|c| class_names.contains(&c.as_str())) {
                    return false;
                }
                pseudo_classes.is_empty() && attrs.is_empty()
            }
        }
    }

    /// Full match with structural/state context.
    pub fn matches_with_context(&self, ctx: &MatchContext<'_>) -> bool {
        match self {
            Selector::Universal => true,
            Selector::Element(e) => e == ctx.element_kind,
            Selector::Class(c) => ctx.class_names.contains(&c.as_str()),
            Selector::Compound { element, classes, pseudo_classes, attrs } => {
                if let Some(e) = element
                    && e != ctx.element_kind
                {
                    return false;
                }
                if !classes.iter().all(|c| ctx.class_names.contains(&c.as_str())) {
                    return false;
                }
                for pc in pseudo_classes {
                    if !match_pseudo_class(pc, ctx) {
                        return false;
                    }
                }
                for attr_sel in attrs {
                    let val = ctx.attributes.iter()
                        .find(|(k, _)| *k == attr_sel.name)
                        .map(|(_, v)| *v);
                    if !attr_sel.matches_value(val) {
                        return false;
                    }
                }
                true
            }
        }
    }
}

fn match_pseudo_class(pc: &PseudoClass, ctx: &MatchContext<'_>) -> bool {
    match pc {
        PseudoClass::Hover => ctx.pseudo_state.hovered,
        PseudoClass::Focus => ctx.pseudo_state.focused,
        PseudoClass::Active => ctx.pseudo_state.active,
        PseudoClass::Visited => false,
        PseudoClass::Disabled => ctx.pseudo_state.disabled,
        PseudoClass::Enabled => !ctx.pseudo_state.disabled,
        PseudoClass::Checked => ctx.pseudo_state.checked,
        PseudoClass::FirstChild => ctx.child_index == 1,
        PseudoClass::LastChild => ctx.child_index == ctx.sibling_count,
        PseudoClass::NthChild(expr) => expr.matches(ctx.child_index),
        PseudoClass::NthLastChild(expr) => {
            let from_end = ctx.sibling_count + 1 - ctx.child_index;
            expr.matches(from_end)
        }
        PseudoClass::OnlyChild => ctx.sibling_count == 1,
        PseudoClass::Empty => ctx.child_count == 0,
        PseudoClass::Not(inner) => !inner.matches_with_context(ctx),
    }
}

pub fn parse_css(source: &str) -> Stylesheet {
    let source = strip_comments(source);
    let mut layer_order: Vec<String> = Vec::new();
    let mut anon_counter: u32 = 0;
    let mut keyframes: Vec<KeyframeAnimation> = Vec::new();
    let mut font_faces: Vec<FontFace> = Vec::new();
    let rules = parse_block(
        &source,
        &mut layer_order,
        &mut anon_counter,
        None,
        &mut keyframes,
        &mut font_faces,
    );
    Stylesheet { layer_order, rules, keyframes, font_faces }
}

/// Parse a block of CSS, which can be the top level or the inside of an @layer.
fn parse_block(
    source: &str,
    layer_order: &mut Vec<String>,
    anon_counter: &mut u32,
    current_layer: Option<&str>,
    keyframes: &mut Vec<KeyframeAnimation>,
    font_faces: &mut Vec<FontFace>,
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
                keyframes,
                font_faces,
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
    keyframes: &mut Vec<KeyframeAnimation>,
    font_faces: &mut Vec<FontFace>,
) -> usize {
    let bytes = source.as_bytes();
    let mut pos = start + 1; // skip @

    let kw_start = pos;
    while pos < bytes.len() && (bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'-') {
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
            let anon_name = format!("__anon_{anon_counter}");
            *anon_counter += 1;
            let full_name = qualify_layer_name(current_layer, &anon_name);
            push_layer_if_new(layer_order, &full_name);
            pos += 1;
            let (block_str, advance) = extract_brace_content(&source[pos..]);
            pos += advance;
            let inner = parse_block(block_str, layer_order, anon_counter, Some(&full_name), keyframes, font_faces);
            rules.extend(inner);
        } else if bytes[pos] == b';' {
            pos += 1;
        } else {
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
                for name in name_part.split(',') {
                    let name = name.trim();
                    if !name.is_empty() {
                        let full = qualify_layer_name(current_layer, name);
                        push_layer_if_new(layer_order, &full);
                    }
                }
                pos = scan + 1;
            } else {
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
                let inner = parse_block(block_str, layer_order, anon_counter, Some(&full_name), keyframes, font_faces);
                rules.extend(inner);
            }
        }
    } else if keyword == "keyframes" {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let name_start = pos;
        while pos < bytes.len() && bytes[pos] != b'{' && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let anim_name = source[name_start..pos].trim().to_string();
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        if pos < bytes.len() {
            pos += 1;
            let (block_str, advance) = extract_brace_content(&source[pos..]);
            pos += advance;
            let stops = parse_keyframe_block(block_str);
            keyframes.push(KeyframeAnimation { name: anim_name, stops });
        }
    } else if keyword == "media" {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let condition_start = pos;
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        let _condition = source[condition_start..pos].trim();
        if pos < bytes.len() {
            pos += 1;
            let (block_str, advance) = extract_brace_content(&source[pos..]);
            pos += advance;
            let inner = parse_block(block_str, layer_order, anon_counter, current_layer, keyframes, font_faces);
            rules.extend(inner);
        }
    } else if keyword == "font-face" {
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        if pos < bytes.len() {
            pos += 1;
            let (block_str, advance) = extract_brace_content(&source[pos..]);
            pos += advance;
            let ff = parse_font_face_block(block_str);
            font_faces.push(ff);
        }
    } else {
        // Other @-rules — skip
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

fn parse_keyframe_block(source: &str) -> Vec<KeyframeStop> {
    let mut stops = Vec::new();
    let bytes = source.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        let sel_start = pos;
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let selector = source[sel_start..pos].trim();
        pos += 1;

        let (block_str, advance) = extract_brace_content(&source[pos..]);
        pos += advance;

        let offsets = parse_keyframe_selectors(selector);
        let style = parse_declarations(block_str);

        for offset in offsets {
            stops.push(KeyframeStop { offset, style: style.clone() });
        }
    }

    stops
}

fn parse_keyframe_selectors(s: &str) -> Vec<f32> {
    s.split(',')
        .filter_map(|part| {
            let part = part.trim();
            match part {
                "from" => Some(0.0),
                "to" => Some(1.0),
                _ => part.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok().map(|v| v / 100.0)),
            }
        })
        .collect()
}

fn parse_font_face_block(source: &str) -> FontFace {
    let mut family = String::new();
    let mut src = String::new();
    let mut weight = None;
    let mut style = None;

    for decl in source.split(';') {
        let decl = decl.trim();
        if let Some(colon) = decl.find(':') {
            let prop = decl[..colon].trim();
            let val = decl[colon + 1..].trim();
            match prop {
                "font-family" => family = val.trim_matches('"').trim_matches('\'').to_string(),
                "src" => src = val.to_string(),
                "font-weight" => weight = Some(val.to_string()),
                "font-style" => style = Some(val.to_string()),
                _ => {}
            }
        }
    }

    FontFace { family, src, weight, style }
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

    // Take only the last simple selector in a combinator chain.
    // Must skip characters inside parentheses (e.g. `:nth-child(2n+1)`).
    let s = rsplit_combinator(s);

    if s.is_empty() {
        return None;
    }

    // Extract attribute selectors `[...]`
    let mut attrs = Vec::new();
    let mut base = String::new();
    let mut chars = s.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch == '[' {
            chars.next();
            let mut attr_str = String::new();
            let mut depth = 1;
            for c in chars.by_ref() {
                if c == ']' {
                    depth -= 1;
                    if depth == 0 { break; }
                } else if c == '[' {
                    depth += 1;
                }
                attr_str.push(c);
            }
            if let Some(attr) = parse_attr_selector(&attr_str) {
                attrs.push(attr);
            }
        } else {
            base.push(ch);
            chars.next();
        }
    }

    // Split pseudo-classes from the base: "div.cls:hover:focus"
    let (main_part, pseudo_classes) = parse_pseudo_classes(&base);
    if main_part.is_empty() && pseudo_classes.is_empty() && attrs.is_empty() {
        return None;
    }

    let parts: Vec<&str> = main_part.split('.').collect();

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

    if classes.is_empty() && pseudo_classes.is_empty() && attrs.is_empty() {
        element.map(Selector::Element)
    } else if element.is_none() && classes.len() == 1 && pseudo_classes.is_empty() && attrs.is_empty() {
        Some(Selector::Class(classes.into_iter().next().unwrap()))
    } else {
        Some(Selector::Compound { element, classes, pseudo_classes, attrs })
    }
}

fn rsplit_combinator(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut paren_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut last_split = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => paren_depth += 1,
            b')' => paren_depth -= 1,
            b'[' => bracket_depth += 1,
            b']' => bracket_depth -= 1,
            b' ' | b'\t' | b'>' | b'+' | b'~' if paren_depth == 0 && bracket_depth == 0 => {
                last_split = Some(i);
            }
            _ => {}
        }
    }
    match last_split {
        Some(pos) => s[pos + 1..].trim(),
        None => s,
    }
}

fn parse_pseudo_classes(s: &str) -> (String, Vec<PseudoClass>) {
    let mut pseudo_classes = Vec::new();

    // Find the first ':' that isn't inside parentheses
    let bytes = s.as_bytes();
    let mut split_pos = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' && (i == 0 || bytes[i - 1] != b':') {
            split_pos = Some(i);
            break;
        }
        i += 1;
    }

    let (main_part, pseudo_str) = match split_pos {
        Some(pos) => (&s[..pos], &s[pos..]),
        None => (s, ""),
    };

    if !pseudo_str.is_empty() {
        let mut rest = pseudo_str;
        while let Some(stripped) = rest.strip_prefix(':') {
            rest = stripped;

            // Extract the pseudo-class name (and optional parenthesized arg)
            let (name, arg, remaining) = extract_pseudo(rest);
            if let Some(pc) = parse_single_pseudo(&name, arg.as_deref()) {
                pseudo_classes.push(pc);
            }
            rest = remaining;
        }
    }

    (main_part.to_string(), pseudo_classes)
}

fn extract_pseudo(s: &str) -> (String, Option<String>, &str) {
    let mut name = String::new();
    let mut chars = s.char_indices();
    let mut end = s.len();

    while let Some((i, c)) = chars.next() {
        if c == '(' {
            let paren_start = i + 1;
            let mut depth = 1;
            let mut paren_end = s.len();
            for (j, ch) in chars.by_ref() {
                if ch == '(' { depth += 1; }
                if ch == ')' {
                    depth -= 1;
                    if depth == 0 {
                        paren_end = j;
                        end = j + 1;
                        break;
                    }
                }
            }
            let arg = s[paren_start..paren_end].trim().to_string();
            return (name, Some(arg), &s[end..]);
        } else if c == ':' || c == '[' || c == ' ' {
            end = i;
            break;
        } else {
            name.push(c);
            end = i + c.len_utf8();
        }
    }

    (name, None, &s[end..])
}

fn parse_single_pseudo(name: &str, arg: Option<&str>) -> Option<PseudoClass> {
    match name {
        "hover" => Some(PseudoClass::Hover),
        "focus" => Some(PseudoClass::Focus),
        "active" => Some(PseudoClass::Active),
        "visited" => Some(PseudoClass::Visited),
        "disabled" => Some(PseudoClass::Disabled),
        "enabled" => Some(PseudoClass::Enabled),
        "checked" => Some(PseudoClass::Checked),
        "first-child" => Some(PseudoClass::FirstChild),
        "last-child" => Some(PseudoClass::LastChild),
        "only-child" => Some(PseudoClass::OnlyChild),
        "empty" => Some(PseudoClass::Empty),
        "nth-child" => {
            let expr = parse_nth_expr(arg.unwrap_or("0"))?;
            Some(PseudoClass::NthChild(expr))
        }
        "nth-last-child" => {
            let expr = parse_nth_expr(arg.unwrap_or("0"))?;
            Some(PseudoClass::NthLastChild(expr))
        }
        "not" => {
            let inner = arg.and_then(parse_single_selector)?;
            Some(PseudoClass::Not(Box::new(inner)))
        }
        _ => None,
    }
}

fn parse_nth_expr(s: &str) -> Option<NthExpr> {
    let s = s.trim();
    match s {
        "odd" => return Some(NthExpr { a: 2, b: 1 }),
        "even" => return Some(NthExpr { a: 2, b: 0 }),
        _ => {}
    }
    if let Ok(n) = s.parse::<i32>() {
        return Some(NthExpr { a: 0, b: n });
    }
    // Parse "An+B" or "An-B"
    if let Some(n_pos) = s.find('n') {
        let a_str = s[..n_pos].trim();
        let a = match a_str {
            "" | "+" => 1,
            "-" => -1,
            _ => a_str.parse().ok()?,
        };
        let rest = s[n_pos + 1..].trim();
        let b = if rest.is_empty() {
            0
        } else {
            rest.replace(' ', "").parse().ok()?
        };
        Some(NthExpr { a, b })
    } else {
        None
    }
}

fn parse_attr_selector(s: &str) -> Option<AttrSelector> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // [attr~=value], [attr|=value], [attr^=value], [attr$=value], [attr*=value], [attr=value]
    let ops = &[("~=", AttrOp::Includes as fn(String) -> AttrOp),
                ("|=", AttrOp::DashMatch as fn(String) -> AttrOp),
                ("^=", AttrOp::StartsWith as fn(String) -> AttrOp),
                ("$=", AttrOp::EndsWith as fn(String) -> AttrOp),
                ("*=", AttrOp::Contains as fn(String) -> AttrOp)];

    for (op_str, constructor) in ops {
        if let Some(pos) = s.find(op_str) {
            let name = s[..pos].trim().to_string();
            let value = unquote(s[pos + op_str.len()..].trim());
            return Some(AttrSelector { name, op: constructor(value) });
        }
    }

    if let Some(pos) = s.find('=') {
        let name = s[..pos].trim().to_string();
        let value = unquote(s[pos + 1..].trim());
        return Some(AttrSelector { name, op: AttrOp::Exact(value) });
    }

    Some(AttrSelector {
        name: s.to_string(),
        op: AttrOp::Exists,
    })
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn parse_declarations(block: &str) -> StyleDecl {
    let mut style = StyleDecl::default();

    // First pass: collect custom properties for var() resolution
    let mut custom_props: HashMap<String, String> = HashMap::new();
    for decl in block.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }
        if let Some(colon_pos) = decl.find(':') {
            let property = decl[..colon_pos].trim();
            if property.starts_with("--") {
                let value = decl[colon_pos + 1..].trim();
                let value = value.trim_end_matches("!important").trim();
                custom_props.insert(property.to_string(), value.to_string());
            }
        }
    }

    // Second pass: apply all properties with var() resolution
    for decl in block.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }

        if let Some(colon_pos) = decl.find(':') {
            let property = decl[..colon_pos].trim();
            let value = decl[colon_pos + 1..].trim();
            let value = value.trim_end_matches("!important").trim();
            let resolved = resolve_var(value, &custom_props);
            apply_css_property(&mut style, property, &resolved);
        }
    }

    style
}

fn apply_css_property(style: &mut StyleDecl, property: &str, value: &str) {
    if property.starts_with("--") {
        let props = style.custom_properties.get_or_insert_with(HashMap::new);
        props.insert(property.to_string(), value.to_string());
        return;
    }
    match property {
        "gap" => style.gap = css_parse_px(value),
        "padding" => style.padding = css_parse_px(value),
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
            style.padding = css_parse_px(value);
        }
        "margin" => style.margin = css_parse_px(value),
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            style.margin = css_parse_px(value);
        }
        "font-size" => style.font_size = css_parse_px(value),
        "font-weight" => style.font_weight = parse_font_weight(value),
        "font-family" => style.font_family = Some(value.trim_matches('"').trim_matches('\'').to_string()),
        "font-style" => style.font_style = Some(value.to_string()),
        "color" => style.color = Some(value.to_string()),
        "background" | "background-color" => style.background = Some(value.to_string()),
        "border-radius" => style.border_radius = css_parse_px(value),
        "border-width" => style.border_width = css_parse_px(value),
        "border-color" => style.border_color = Some(value.to_string()),
        "align-items" => style.align_items = Some(value.to_string()),
        "align-self" => style.align_self = Some(value.to_string()),
        "align-content" => style.align_content = Some(value.to_string()),
        "justify-content" => style.justify_content = Some(value.to_string()),
        "width" => style.width = Some(value.to_string()),
        "height" => style.height = Some(value.to_string()),
        "min-width" => style.min_width = Some(value.to_string()),
        "min-height" => style.min_height = Some(value.to_string()),
        "max-width" => style.max_width = Some(value.to_string()),
        "max-height" => style.max_height = Some(value.to_string()),
        "flex-grow" => style.flex_grow = value.parse().ok(),
        "flex-shrink" => style.flex_shrink = value.parse().ok(),
        "flex-basis" => style.flex_basis = Some(value.to_string()),
        "flex-direction" => style.flex_direction = Some(value.to_string()),
        "flex-wrap" => style.flex_wrap = Some(value.to_string()),
        "flex" => parse_flex_shorthand(style, value),
        "order" => style.order = value.parse().ok(),
        "position" => style.position = Some(value.to_string()),
        "top" => style.top = Some(value.to_string()),
        "right" => style.right = Some(value.to_string()),
        "bottom" => style.bottom = Some(value.to_string()),
        "left" => style.left = Some(value.to_string()),
        "z-index" => style.z_index = value.parse().ok(),
        "overflow" => style.overflow = Some(value.to_string()),
        "display" => style.display = Some(value.to_string()),
        "opacity" => style.opacity = value.parse().ok(),
        "visibility" => style.visibility = Some(value.to_string()),
        "cursor" => style.cursor = Some(value.to_string()),
        "pointer-events" => style.pointer_events = Some(value.to_string()),
        "user-select" => style.user_select = Some(value.to_string()),
        "text-align" => style.text_align = Some(value.to_string()),
        "white-space" => style.white_space = Some(value.to_string()),
        "line-height" => style.line_height = css_parse_px(value).or_else(|| value.parse().ok()),
        "letter-spacing" => style.letter_spacing = css_parse_px(value),
        "text-decoration" => style.text_decoration = Some(value.to_string()),
        "text-overflow" => style.text_overflow = Some(value.to_string()),
        "word-break" => style.word_break = Some(value.to_string()),
        "outline-width" => style.outline_width = css_parse_px(value),
        "outline-color" => style.outline_color = Some(value.to_string()),
        "outline-style" => style.outline_style = Some(value.to_string()),
        "outline" => parse_outline_shorthand(style, value),
        "transform" => style.transform = Some(value.to_string()),
        "transition" => style.transition = Some(value.to_string()),
        "box-shadow" => style.box_shadow = Some(value.to_string()),
        "border" => parse_border_shorthand(style, value),
        _ => {}
    }
}

fn parse_flex_shorthand(style: &mut StyleDecl, value: &str) {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.len() {
        1 => {
            if let Ok(v) = parts[0].parse::<f32>() {
                style.flex_grow = Some(v);
            }
        }
        2 => {
            style.flex_grow = parts[0].parse().ok();
            style.flex_shrink = parts[1].parse().ok();
        }
        3 => {
            style.flex_grow = parts[0].parse().ok();
            style.flex_shrink = parts[1].parse().ok();
            style.flex_basis = Some(parts[2].to_string());
        }
        _ => {}
    }
}

fn parse_outline_shorthand(style: &mut StyleDecl, value: &str) {
    for part in value.split_whitespace() {
        if let Some(px) = css_parse_px(part) {
            style.outline_width = Some(px);
        } else if part.starts_with('#') || part.starts_with("rgb") {
            style.outline_color = Some(part.to_string());
        } else {
            style.outline_style = Some(part.to_string());
        }
    }
}

fn resolve_var(value: &str, custom_props: &HashMap<String, String>) -> String {
    if !value.contains("var(") {
        return value.to_string();
    }
    let mut result = value.to_string();
    // Iteratively resolve var() references (handles nested var())
    for _ in 0..10 {
        let Some(start) = result.find("var(") else {
            break;
        };
        let after = &result[start + 4..];
        let mut depth = 1i32;
        let mut end = after.len();
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            break;
        }
        let inner = after[..end].trim();
        let (var_name, fallback) = match inner.find(',') {
            Some(comma) => (inner[..comma].trim(), Some(inner[comma + 1..].trim())),
            None => (inner, None),
        };
        let replacement = custom_props
            .get(var_name)
            .map(|s| s.as_str())
            .or(fallback)
            .unwrap_or("");
        result = format!("{}{}{}", &result[..start], replacement, &after[end + 1..]);
    }
    result
}

fn css_parse_px(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    if trimmed.starts_with("calc(") {
        return None;
    }
    let v = trimmed.trim_end_matches("px");
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
            Selector::Compound { element, classes, .. } => {
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
            pseudo_classes: vec![],
            attrs: vec![],
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
    fn media_rules_parsed() {
        let css = r#"
            @media (max-width: 600px) {
                .title { font-size: 18px; }
            }
            .body { gap: 8; }
        "#;
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[1].style.gap, Some(8.0));
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
    fn layer_with_media_parsed() {
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
        assert_eq!(sheet.rules.len(), 3);
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

    // ── Pseudo-class selector tests ──

    #[test]
    fn parse_hover_pseudo() {
        let css = ".btn:hover { background: #fff; }";
        let sheet = parse_css(css);
        assert_eq!(sheet.rules.len(), 1);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { classes, pseudo_classes, .. } => {
                assert_eq!(classes, &["btn"]);
                assert_eq!(pseudo_classes.len(), 1);
                assert!(matches!(pseudo_classes[0], PseudoClass::Hover));
            }
            _ => panic!("expected Compound selector with pseudo-class"),
        }
    }

    #[test]
    fn parse_multiple_pseudo_classes() {
        let css = "button:hover:active { color: red; }";
        let sheet = parse_css(css);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { element, pseudo_classes, .. } => {
                assert_eq!(element.as_deref(), Some("button"));
                assert_eq!(pseudo_classes.len(), 2);
                assert!(matches!(pseudo_classes[0], PseudoClass::Hover));
                assert!(matches!(pseudo_classes[1], PseudoClass::Active));
            }
            _ => panic!("expected Compound"),
        }
    }

    #[test]
    fn parse_first_child() {
        let css = "li:first-child { font-weight: bold; }";
        let sheet = parse_css(css);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { pseudo_classes, .. } => {
                assert!(matches!(pseudo_classes[0], PseudoClass::FirstChild));
            }
            _ => panic!("expected Compound"),
        }
    }

    #[test]
    fn parse_nth_child() {
        let css = "tr:nth-child(2n+1) { background: #eee; }";
        let sheet = parse_css(css);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { pseudo_classes, .. } => {
                match &pseudo_classes[0] {
                    PseudoClass::NthChild(expr) => {
                        assert_eq!(expr.a, 2);
                        assert_eq!(expr.b, 1);
                    }
                    _ => panic!("expected NthChild"),
                }
            }
            _ => panic!("expected Compound"),
        }
    }

    #[test]
    fn nth_expr_odd_even() {
        let odd = parse_nth_expr("odd").unwrap();
        assert!(odd.matches(1));
        assert!(!odd.matches(2));
        assert!(odd.matches(3));

        let even = parse_nth_expr("even").unwrap();
        assert!(!even.matches(1));
        assert!(even.matches(2));
        assert!(!even.matches(3));
    }

    #[test]
    fn nth_expr_constant() {
        let expr = parse_nth_expr("3").unwrap();
        assert!(!expr.matches(1));
        assert!(!expr.matches(2));
        assert!(expr.matches(3));
        assert!(!expr.matches(4));
    }

    #[test]
    fn pseudo_class_context_matching() {
        let sel = Selector::Compound {
            element: Some("div".to_string()),
            classes: vec![],
            pseudo_classes: vec![PseudoClass::Hover],
            attrs: vec![],
        };
        let ctx_no_hover = MatchContext {
            element_kind: "div",
            class_names: &[],
            attributes: &[],
            child_index: 1,
            sibling_count: 3,
            child_count: 0,
            pseudo_state: PseudoState::default(),
        };
        assert!(!sel.matches_with_context(&ctx_no_hover));

        let ctx_hover = MatchContext {
            pseudo_state: PseudoState { hovered: true, ..PseudoState::default() },
            ..ctx_no_hover
        };
        assert!(sel.matches_with_context(&ctx_hover));
    }

    // ── Attribute selector tests ──

    #[test]
    fn parse_attr_exists() {
        let css = "input[disabled] { opacity: 0.5; }";
        let sheet = parse_css(css);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { element, attrs, .. } => {
                assert_eq!(element.as_deref(), Some("input"));
                assert_eq!(attrs.len(), 1);
                assert_eq!(attrs[0].name, "disabled");
                assert!(matches!(attrs[0].op, AttrOp::Exists));
            }
            _ => panic!("expected Compound"),
        }
    }

    #[test]
    fn parse_attr_exact() {
        let css = r#"input[type="text"] { border: 1px; }"#;
        let sheet = parse_css(css);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { attrs, .. } => {
                assert_eq!(attrs[0].name, "type");
                match &attrs[0].op {
                    AttrOp::Exact(v) => assert_eq!(v, "text"),
                    _ => panic!("expected Exact"),
                }
            }
            _ => panic!("expected Compound"),
        }
    }

    #[test]
    fn parse_attr_contains() {
        let css = r#"a[href*="example"] { color: blue; }"#;
        let sheet = parse_css(css);
        match &sheet.rules[0].selectors[0] {
            Selector::Compound { attrs, .. } => {
                match &attrs[0].op {
                    AttrOp::Contains(v) => assert_eq!(v, "example"),
                    _ => panic!("expected Contains"),
                }
            }
            _ => panic!("expected Compound"),
        }
    }

    #[test]
    fn attr_selector_matching() {
        let attr = AttrSelector { name: "type".to_string(), op: AttrOp::Exact("text".to_string()) };
        assert!(attr.matches_value(Some("text")));
        assert!(!attr.matches_value(Some("password")));
        assert!(!attr.matches_value(None));

        let exists = AttrSelector { name: "disabled".to_string(), op: AttrOp::Exists };
        assert!(exists.matches_value(Some("")));
        assert!(exists.matches_value(Some("true")));
        assert!(!exists.matches_value(None));
    }

    #[test]
    fn attr_starts_with() {
        let attr = AttrSelector { name: "class".to_string(), op: AttrOp::StartsWith("btn".to_string()) };
        assert!(attr.matches_value(Some("btn-primary")));
        assert!(!attr.matches_value(Some("card-btn")));
    }

    #[test]
    fn attr_includes() {
        let attr = AttrSelector { name: "class".to_string(), op: AttrOp::Includes("active".to_string()) };
        assert!(attr.matches_value(Some("btn active large")));
        assert!(!attr.matches_value(Some("inactive")));
    }
}
